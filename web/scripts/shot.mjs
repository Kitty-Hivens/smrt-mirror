// Drive the panel headless and capture screenshots, including the signed-in
// operator views and a responsive sweep across the layout breakpoints.
//
// Env: FIREFOX or CHROME, BASE, SESSION, OUT (see scripts/lib/harness.mjs), plus
//      WIDTHS -- comma-separated px for the responsive sweep.
//
// Without SESSION it captures the login page and stops, which is the honest
// half of the job for a caller who has not copied a cookie yet.
import { launch, signedIn, clickByText, go, shoot, sleep, BASE, OUT, DSF, SESSION }
  from './lib/harness.mjs';

// Defaults straddle both layout breaks: 320/375/560 -> phone drawer, 768 ->
// tablet strip, 1024/1440 -> desktop sidebar (see --bp-sm 560 / --bp-md 768).
const BP_WIDTHS = (process.env.WIDTHS ?? '320,375,560,768,1024,1440')
  .split(',')
  .map((s) => Number(s.trim()))
  .filter((n) => n > 0);

const browser = await launch({ width: 1180, height: 760 });

// Shoot the current view at every breakpoint width. CSS relays out on resize,
// so no reload is needed -- capture whatever is on screen as it reflows.
async function sweep(page, name, opts = {}) {
  const { height = 900, scrollTo = null } = opts;
  for (const w of BP_WIDTHS) {
    await page.setViewport({ width: w, height, deviceScaleFactor: 1 });
    await sleep(300);
    if (scrollTo) {
      await page.evaluate((sel) => {
        document.querySelector(sel)?.scrollIntoView({ block: 'center' });
      }, scrollTo);
      await sleep(150);
    }
    await page.screenshot({ path: `${OUT}/bp-${name}-${w}.png` });
  }
}

try {
  // The login page, unauthenticated. Its own locale switch is the one `.loc`
  // that exists -- the shell's was removed, which is why the signed-in half
  // sets the locale in storage instead of clicking for it.
  const guest = await browser.newPage();
  await guest.goto(`${BASE}/`, { waitUntil: 'networkidle0' });
  await guest.waitForSelector('.gh', { timeout: 6000 }).catch(() => {});
  await guest.screenshot({ path: `${OUT}/smrt-login.png` });
  if (await clickByText(guest, '.loc', 'EN')) {
    await guest.screenshot({ path: `${OUT}/smrt-login-en.png` });
    await clickByText(guest, '.loc', 'RU');
  }
  await sweep(guest, 'login');
  await guest.close();

  if (!SESSION) {
    console.log('SESSION not set -- captured the login page only.');
    console.log('Sign in, copy the smrt_session cookie, and re-run with SESSION=<value>.');
  } else {
    const page = await signedIn(browser);

    // single-viewport reference shots
    await page.setViewport({ width: 1180, height: 760, deviceScaleFactor: DSF });
    await go(page, 'Overview');
    await shoot(page, 'smrt-overview');
    await go(page, 'Packs');
    await shoot(page, 'smrt-packs');

    // ── responsive breakpoint sweeps ───────────────────────────────────────
    // Navigate at desktop width (the rail collapses to a drawer on phones and
    // is not clickable there), then resize down and shoot.
    const desktop = () => page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });

    await desktop();
    await go(page, 'Overview');
    await sweep(page, 'overview');

    await desktop();
    await go(page, 'Packs');
    await sweep(page, 'packs');

    // drawer open on a phone: the burger toggles the off-canvas rail over a scrim
    await page.setViewport({ width: 375, height: 812, deviceScaleFactor: 1 });
    await sleep(300);
    const burger = await page.$('.burger');
    if (burger) {
      await burger.click();
      await sleep(350);
      await page.screenshot({ path: `${OUT}/bp-drawer-open-375.png` });
      await page.keyboard.press('Escape');
      await sleep(250);
    }

    // the pack editor: the mod row and the basics grid both reflow
    await desktop();
    await go(page, 'Packs');
    const packRow = await page.$('tr.clickable');
    if (packRow) {
      await packRow.click();
      await sleep(800);
      await shoot(page, 'smrt-pack-config');
      await sweep(page, 'pack-config');
      await sweep(page, 'modrow', { scrollTo: '.modrow' });

      // the picker modal: its filter row wraps on phones
      await desktop();
      if (await clickByText(page, 'button', 'Add a mod')) {
        await page.waitForSelector('[role=dialog]', { timeout: 4000 }).catch(() => {});
        await sleep(300);
        await sweep(page, 'mod-picker');
      }
    } else {
      console.log('no packs found -- skipped the editor sweeps.');
    }
  }

  console.log('shots ->', OUT, '| breakpoints:', BP_WIDTHS.join(', '));
} finally {
  await browser.close();
}
