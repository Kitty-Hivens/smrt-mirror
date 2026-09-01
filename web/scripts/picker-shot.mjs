// The search-to-add picker, with results on screen.
//
// It used to open the Modrinth-only picker. That is one half of what the button
// does now: "Add a mod" searches the mirror's own registry and Modrinth at once
// (#101) and each hit says which it came from, which is the thing worth a
// screenshot. Env: QUERY (default `appleskin`).
import { launch, signedIn, openFirstPack, clickByText, shoot, sleep } from './lib/harness.mjs';

const QUERY = process.env.QUERY ?? 'appleskin';

const browser = await launch({ width: 1280, height: 900 });
try {
  const page = await signedIn(browser);
  if (!(await openFirstPack(page))) {
    console.log('the mirror has no packs -- nothing to shoot.');
  } else if (!(await clickByText(page, 'button', 'Add a mod'))) {
    throw new Error('no "Add a mod" button on the editor\'s Config tab');
  } else {
    await page.waitForSelector('input', { timeout: 4000 });
    // the picker's own search box is the first input inside the dialog
    const box = await page.$('[role=dialog] input');
    if (!box) throw new Error('the picker opened without a search box');
    await box.type(QUERY);
    // the search is debounced, then answers from the registry and upstream
    await sleep(2500);
    await shoot(page, 'smrt-picker');
    const hits = await page.$$eval('[role=dialog] .hit', (els) => els.length);
    console.log(`picker: ${hits} hit(s) for ${JSON.stringify(QUERY)}`);
  }
} finally {
  await browser.close();
}
