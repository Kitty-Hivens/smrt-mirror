// The pack editor's Branding tab: the drop zone and whatever the pack already
// holds. `FIREFOX=/usr/bin/firefox BASE=... SESSION=... node scripts/branding-shot.mjs`
import { launch, signedIn, openFirstPack, editorTab, shoot, sleep } from './lib/harness.mjs';

const browser = await launch({ width: 1280, height: 900 });
try {
  const page = await signedIn(browser);
  if (!(await openFirstPack(page))) {
    console.log('the mirror has no packs -- nothing to shoot.');
  } else {
    if (!(await editorTab(page, 'Branding'))) throw new Error('no Branding tab in the editor');
    await sleep(700);
    await shoot(page, 'smrt-branding');
    const imgs = await page.$$eval('img', (els) => els.length);
    const drops = await page.$$eval('input[type=file]', (els) => els.length);
    console.log(`branding: ${imgs} image(s), ${drops} drop target(s)`);
  }
} finally {
  await browser.close();
}
