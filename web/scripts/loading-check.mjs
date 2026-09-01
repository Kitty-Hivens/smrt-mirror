// What the panel does while it is waiting, checked rather than looked at.
//
// Two properties, both of which used to be wrong and neither of which shows up
// in a screenshot of a loaded page:
//
//   1. A list holds the shape of the rows that are coming, and only once the
//      wait is long enough to be worth drawing. Too early is a flash of grey;
//      the wrong pitch is a hundred pixels of jump when the rows land.
//   2. A filter says it has heard the keystroke on the frame it arrives, not a
//      debounce and a round trip later. The debounce is deliberate -- it is what
//      keeps a five-letter word from being five searches -- but for as long as
//      it runs, the list on screen answers a question nobody is asking.
//
// The mirror it runs against needs no particular contents: the rows are supplied
// by a stub in front of `fetch`, because neither property is about where the
// rows came from, and the delay is imposed there too -- a wait too short to see
// is exactly the one the design does not draw. Exit code 1 on a regression.
import { launch, signedIn, sleep, BASE } from './lib/harness.mjs';

// Long enough that every threshold under test is comfortably inside it.
const SERVER_MS = 900;
// What Skeleton holds back for. Kept here so a change to it fails this check
// rather than passing silently.
const THRESHOLD_MS = 250;

const failures = [];
function check(ok, what, detail) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${what}${detail ? ` -- ${detail}` : ''}`);
  if (!ok) failures.push(what);
}

const mods = (n, tag) =>
  Array.from({ length: n }, (_, i) => ({
    mod_id: i + 1,
    name: `${tag} mod ${i + 1}`,
    author: 'someone',
    modid: `demo${i}`,
    loaders: ['forge'],
    mc_versions: ['1.20.1'],
    version_count: 3,
    icon_sha1: null,
    modrinth_project_id: null,
  }));

/// Answer the registry listing slowly, and with a different set the second time
/// so a narrowed search is visibly a different answer.
async function stubListing(page, delay, first, later) {
  await page.evaluateOnNewDocument(
    (ms, a, b) => {
      const real = window.fetch;
      let seen = 0;
      window.fetch = async (input, init) => {
        const url = String(typeof input === 'string' ? input : input.url);
        if (url.includes('/v1/registry/mods')) {
          await new Promise((r) => setTimeout(r, ms));
          return new Response(JSON.stringify(seen++ === 0 ? a : b), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return real(input, init);
      };
    },
    delay,
    first,
    later,
  );
}

const browser = await launch({ width: 1180, height: 820 });
let page;
try {
  page = await signedIn(browser);
  await stubListing(page, SERVER_MS, mods(6, 'Cached'), mods(2, 'Filtered'));
  await page.goto(`${BASE}/`, { waitUntil: 'networkidle0' });
  await page.waitForSelector('.rail .item');

  const rail = await page.evaluateHandle(() =>
    [...document.querySelectorAll('.item')].find((b) => b.textContent.trim() === 'Mods'),
  );
  const entry = rail.asElement();
  if (!entry) throw new Error('no rail entry labelled "Mods" -- is the interface in English?');
  await entry.click();

  // --- 1. the wait has a shape, and not before it is worth one ---
  await sleep(THRESHOLD_MS - 130);
  const early = await page.evaluate(() => document.querySelectorAll('.sk').length);
  check(early === 0, 'nothing is drawn before the threshold', `${early} blocks`);

  // Polled rather than sampled once: the live region is filled a tick after it
  // mounts, which is what makes a screen reader read it at all, and a single
  // sample lands on either side of that tick depending on how fast the section's
  // chunk arrived. `.vh` picks the skeleton's own region -- the toaster and the
  // suspension banner are `role=status` too.
  const saidMs = await page.evaluate(async () => {
    const t0 = performance.now();
    for (let i = 0; i < 60; i++) {
      const said = document.querySelector('[role=status].vh')?.textContent?.trim();
      if (said) return { ms: Math.round(performance.now() - t0), said };
      await new Promise((r) => setTimeout(r, 10));
    }
    return null;
  });
  check(
    saidMs !== null && saidMs.ms < THRESHOLD_MS,
    'a screen reader is told before there is anything to look at',
    saidMs ? `${JSON.stringify(saidMs.said)} after ${saidMs.ms}ms` : 'never said',
  );

  await sleep(320);
  const drawn = await page.evaluate(() => {
    const block = document.querySelector('.skel > *');
    return {
      blocks: document.querySelectorAll('.sk').length,
      pitch: block ? Math.round(block.getBoundingClientRect().height) : null,
      shimmer: block ? getComputedStyle(block.querySelector('.sk') ?? block).animationName : null,
    };
  });
  check(drawn.blocks > 0, 'past the threshold the shape is drawn', `${drawn.blocks} blocks`);
  check(drawn.shimmer === 'sk-pulse', 'the blocks shimmer', String(drawn.shimmer));

  // --- 2. the rows land where the placeholders were ---
  await sleep(SERVER_MS);
  const landed = await page.evaluate(() => ({
    blocks: document.querySelectorAll('.sk').length,
    rows: document.querySelectorAll('.mod').length,
    pitch: Math.round(document.querySelector('.mod')?.getBoundingClientRect().height ?? 0),
  }));
  check(landed.blocks === 0 && landed.rows === 6, 'the rows replace the blocks', `${landed.rows} rows`);
  const drift = Math.abs(landed.pitch - drawn.pitch);
  check(drift <= 4, 'the placeholder is the height of the row', `${drawn.pitch}px vs ${landed.pitch}px`);

  // --- 3. a keystroke is acknowledged on the frame it arrives ---
  const input = await page.$('.filters input');
  if (!input) throw new Error('no filter input on the registry browser');
  await input.click();
  await input.type('c');
  const ackMs = await page.evaluate(async () => {
    const el = document.querySelector('.modlist');
    const t0 = performance.now();
    for (let i = 0; i < 180; i++) {
      if (el?.classList.contains('stale')) return Math.round(performance.now() - t0);
      await new Promise((r) => requestAnimationFrame(r));
    }
    return null;
  });
  check(ackMs !== null && ackMs < 100, 'the list says it heard the keystroke', `${ackMs}ms`);

  await sleep(140);
  const dimmed = await page.evaluate(
    () => +getComputedStyle(document.querySelector('.modlist')).opacity,
  );
  check(dimmed < 0.9, 'and stops claiming to be current', `opacity ${dimmed}`);

  // --- 4. the newer answer clears it ---
  await sleep(SERVER_MS + 400);
  const settled = await page.evaluate(() => ({
    rows: document.querySelectorAll('.mod').length,
    opacity: +getComputedStyle(document.querySelector('.modlist')).opacity,
  }));
  check(
    settled.rows === 2 && settled.opacity === 1,
    'the answer arrives and the list is current again',
    `${settled.rows} rows at opacity ${settled.opacity}`,
  );
} finally {
  await browser.close();
}

if (failures.length) {
  console.error(`\n${failures.length} of the panel's waiting behaviours regressed`);
  process.exit(1);
}
console.log('\nthe panel waits the way it is meant to');
