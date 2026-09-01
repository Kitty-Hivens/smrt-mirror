// Opening the panel, signed in, for the scripts that drive it.
//
// Six scripts carried their own copy of "launch a browser and log in", and five
// of them still typed a token into the form that has answered 410 since sign-in
// moved to GitHub OAuth. They had been dead for as long as that, and nothing
// said so: none of them is wired into `pnpm`, so they are only ever run by hand,
// and a hand that stops running them stops hearing about them. One copy now --
// the next auth change breaks this file, not six.
//
// Env, shared by every caller:
//   FIREFOX or CHROME   browser binary (one is required)
//   BASE                panel origin (default http://127.0.0.1:9000)
//   SESSION             the `smrt_session` cookie value, for the signed-in views
//   OUT                 where screenshots land (default /tmp)
//
// Getting a SESSION: sign in to the panel in a real browser and copy the cookie
// (DevTools > Application > Cookies -- it is HttpOnly, so `document.cookie` will
// not show it). Against a throwaway local mirror you can also mint one straight
// into `accounts.db`; see docs/development.md.
import puppeteer from 'puppeteer-core';
import { mkdirSync } from 'node:fs';

const FIREFOX = process.env.FIREFOX;
const CHROME = process.env.CHROME;
const useFirefox = !!FIREFOX;
const EXE = FIREFOX ?? CHROME;

export const BASE = (process.env.BASE ?? 'http://127.0.0.1:9000').replace(/\/+$/, '');
export const SESSION = process.env.SESSION ?? '';
export const OUT = process.env.OUT ?? '/tmp';
// Firefox over WebDriver BiDi is most reliable at 1x; Chrome renders 2x.
export const DSF = useFirefox ? 1 : 2;

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// Launch a browser. Firefox gets a throwaway profile of its own, so it neither
/// touches nor needs you to close one you already have open.
export async function launch(viewport = { width: 1280, height: 900 }) {
  if (!EXE) {
    console.error('set FIREFOX=/usr/bin/firefox (or CHROME=/path/to/chromium)');
    process.exit(1);
  }
  mkdirSync(OUT, { recursive: true });
  return puppeteer.launch({
    executablePath: EXE,
    browser: useFirefox ? 'firefox' : 'chrome',
    headless: true,
    // Chrome-only flags; Firefox needs none of them.
    args: useFirefox ? [] : ['--no-sandbox', '--disable-gpu', '--hide-scrollbars'],
    defaultViewport: { deviceScaleFactor: DSF, ...viewport },
  });
}

/// Put the session in the cookie jar. Firefox's BiDi backend wants an explicit
/// `domain` -- it will not derive one from `url` the way Chrome's CDP does.
async function injectSession(browser, page, value) {
  const cookie = {
    name: 'smrt_session',
    value,
    domain: new URL(BASE).hostname,
    path: '/',
    httpOnly: true,
    secure: BASE.startsWith('https'),
    sameSite: 'Strict',
  };
  if (typeof browser.setCookie === 'function') await browser.setCookie(cookie);
  else await page.setCookie(cookie);
}

/// Ask the API who we are, from the page's own origin so the cookie rides along.
export async function whoAmI(page) {
  return page.evaluate(async (base) => {
    try {
      const r = await fetch(`${base}/v1/me`, { credentials: 'include' });
      if (!r.ok) return { ok: false, detail: String(r.status) };
      const j = await r.json();
      return { ok: !!j.authenticated, detail: `${j.login} / ${j.role}` };
    } catch (e) {
      return { ok: false, detail: `fetch failed: ${e}` };
    }
  }, BASE);
}

/// A page on the panel's origin, signed in, with the rail rendered and the
/// interface in English so labels are deterministic to click by.
///
/// Throws with a diagnosis rather than timing out somewhere deeper: a stale
/// cookie used to surface as "waiting for selector .rail .item", which names
/// neither the cause nor the fix.
export async function signedIn(browser) {
  const page = await browser.newPage();
  await page.goto(`${BASE}/`, { waitUntil: 'networkidle0' });
  if (!SESSION) {
    throw new Error(
      'SESSION is not set. Sign in to the panel, copy the smrt_session cookie ' +
        '(DevTools > Application > Cookies) and re-run with SESSION=<value>.',
    );
  }
  await injectSession(browser, page, SESSION);
  let auth = await whoAmI(page);
  if (!auth.ok) {
    // Some driver/browser combinations do not honour the jar entry. The server
    // only reads the cookie's value, so a plain one on the origin does as well.
    await page.evaluate((v) => {
      const secure = location.protocol === 'https:' ? '; Secure' : '';
      document.cookie = `smrt_session=${v}; path=/; SameSite=Strict${secure}`;
    }, SESSION);
    auth = await whoAmI(page);
  }
  if (!auth.ok) {
    await page.screenshot({ path: `${OUT}/auth-failed.png` }).catch(() => {});
    throw new Error(
      `not signed in: /v1/me -> ${auth.detail}\n` +
        '  401 => the SESSION value is stale or mistyped; copy a fresh smrt_session.\n' +
        `  also check BASE (${BASE}) is the exact origin the cookie came from.\n` +
        `  wrote ${OUT}/auth-failed.png -- what rendered instead of the panel.`,
    );
  }
  // English, so labels are deterministic to click by. Set in storage rather
  // than clicked: the switch that used to sit in the shell (`.loc`) now lives
  // on the login page and in Settings, so a click on it after signing in
  // matches nothing and fails silently -- which is how a script ends up
  // navigating by English labels through a Russian rail and screenshotting
  // whatever was already on screen.
  await page.evaluate(() => {
    try {
      localStorage.setItem('smrt.locale', 'en');
    } catch {
      // blocked storage: the labels stay in the default locale, and a caller
      // clicking by English label will say so rather than shoot the wrong view
    }
  });
  await page.goto(`${BASE}/`, { waitUntil: 'networkidle0' });
  await page.waitForSelector('.rail .item', { timeout: 12000 });
  await sleep(300);
  console.log('signed in as', auth.detail);
  return page;
}

/// Click the one element matching `selector` whose text is exactly `text`.
/// Returns whether anything matched, so a caller can skip rather than hang.
export async function clickByText(page, selector, text) {
  const handle = await page.evaluateHandle(
    (sel, t) => [...document.querySelectorAll(sel)].find((b) => b.textContent.trim() === t),
    selector,
    text,
  );
  const el = handle.asElement();
  if (el) {
    await el.click();
    await sleep(350);
  }
  return !!el;
}

/// Move to a section by its rail label (English).
export async function go(page, label) {
  const ok = await clickByText(page, '.item', label);
  if (!ok) throw new Error(`no rail entry labelled ${JSON.stringify(label)}`);
  await sleep(400);
  return ok;
}

/// Open the first pack in the packs table, or `false` when the mirror has none.
export async function openFirstPack(page) {
  await go(page, 'Packs');
  const row = await page.$('tr.clickable');
  if (!row) return false;
  await row.click();
  await sleep(800);
  return true;
}

/// Switch the open pack editor to one of its tabs by label (English).
export async function editorTab(page, label) {
  // The tabs are a Select on narrow layouts and a strip on wide ones; both
  // render the label as the accessible name of something clickable.
  if (await clickByText(page, '[role=tab]', label)) return true;
  if (await clickByText(page, 'button', label)) return true;
  return false;
}

export async function shoot(page, name) {
  await page.screenshot({ path: `${OUT}/${name}.png` });
  console.log('wrote', `${OUT}/${name}.png`);
}
