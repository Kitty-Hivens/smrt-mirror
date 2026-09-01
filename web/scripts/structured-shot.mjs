// The pack editor's Config tab: the per-mod table and the pack's own fields.
//
// This used to open a "Curator" tab and its "Structured view". Neither exists:
// the curator TOML and the tab that edited it are gone, and the per-mod rows
// they became live on Config. Retargeted rather than deleted -- the thing worth
// looking at is still the per-mod table, it just moved.
import { launch, signedIn, openFirstPack, editorTab, shoot, sleep } from './lib/harness.mjs';

const browser = await launch({ width: 1280, height: 940 });
try {
  const page = await signedIn(browser);
  if (!(await openFirstPack(page))) {
    console.log('the mirror has no packs -- nothing to shoot.');
  } else {
    if (!(await editorTab(page, 'Config'))) throw new Error('no Config tab in the editor');
    await sleep(700);
    await shoot(page, 'smrt-pack-config');
    const rows = await page.$$eval('.modrow', (els) => els.length);
    console.log('per-mod rows:', rows);
  }
} finally {
  await browser.close();
}
