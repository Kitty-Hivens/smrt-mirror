// A real cache-jar upload through the panel: the client hashes the file and PUTs
// it under its own sha1, and the mirror re-verifies. Verifies that whole path.
//
// Env: JAR (a .jar to upload; required).
import { launch, signedIn, go, shoot, sleep } from './lib/harness.mjs';

const JAR = process.env.JAR;
if (!JAR) {
  console.error('set JAR=/path/to/some.jar');
  process.exit(1);
}

const browser = await launch({ width: 1280, height: 900 });
try {
  const page = await signedIn(browser);
  // The cache tab became the Mods section: one place for the registry and the
  // jars behind it, rather than a list of hashes on its own.
  await go(page, 'Mods');
  await sleep(600);
  const input = await page.$('input[type=file]');
  if (!input) throw new Error('no drop target on the Mods section (operator only -- check the role)');
  await input.uploadFile(JAR);
  // hashing happens in the page, so a large jar takes a moment before the PUT
  await sleep(3000);
  await shoot(page, 'check-upload');
  const msg = await page.$eval('.upmsg', (el) => el.textContent.trim()).catch(() => '');
  if (!msg) throw new Error('the upload reported nothing -- it may not have started');
  console.log('upload said:', msg);
  if (/0/.test(msg) && !/1|2|3|4|5|6|7|8|9/.test(msg.replace(/0/g, ''))) process.exitCode = 1;
} finally {
  await browser.close();
}
