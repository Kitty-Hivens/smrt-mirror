// The authoring loop, driven end to end: open a pack, declare a checkpoint,
// build it, and wait for the live log to reach a terminal status.
//
// A check rather than a screenshot -- it answers whether the whole GUI path
// still works, and says which step it stopped at when it does not. Exit code 1
// on a build that fails, so it can be run in anger.
import { launch, signedIn, openFirstPack, editorTab, clickByText, shoot, sleep }
  from './lib/harness.mjs';

/// Wait for the live log to settle. The status rides in the log's own class
/// (`.st.ok` / `.st.bad`), which is where the component puts it -- no test seam
/// needed, and no scraping of prose that is localised.
async function awaitBuild(page, timeoutMs = 360_000) {
  const started = Date.now();
  // A build waits out a running or pending harvest before it classifies, capped
  // at five minutes -- so the wait here has to be longer than that cap, or a
  // healthy build on a freshly-started mirror reads as a hang.
  let sawLog = false;
  while (Date.now() - started < timeoutMs) {
    const state = await page.evaluate(() => {
      const st = document.querySelector('.jl .st');
      if (!st) return null;
      window.__sawLog = true;
      const done = st.classList.contains('ok');
      const failed = st.classList.contains('bad');
      if (!done && !failed) return null;
      const log = document.querySelector('.jl .log');
      return { status: done ? 'done' : 'failed', log: (log?.textContent ?? '').split('\n') };
    });
    if (state) return state;
    sawLog ||= await page.evaluate(() => !!document.querySelector('.jl'));
    await sleep(500);
  }
  throw new Error(
    sawLog
      ? `the build did not settle within ${timeoutMs / 1000}s`
      : 'no build log ever appeared -- the build was never started (the button did nothing)',
  );
}

const browser = await launch({ width: 1280, height: 900 });
try {
  const page = await signedIn(browser);
  if (!(await openFirstPack(page))) {
    console.log('the mirror has no packs -- nothing to drive.');
  } else {
    await editorTab(page, 'Config');
    await sleep(500);
    await shoot(page, 'check-config');

    if (!(await editorTab(page, 'Build'))) throw new Error('no Build tab in the editor');
    await sleep(600);

    // A build is made from a checkpoint, so a pack with uncommitted work is
    // committed first -- which is what the console's own button does in one
    // press. Either way the message box has to be filled.
    // The commit subject, not the version field or a release note: the build
    // refuses without one and says so in a toast, which is a build that never
    // starts and a check that waits for a log that never appears.
    const box = await page.$('.declare .lines input');
    if (!box) throw new Error('no commit-message box on the Build tab');
    await box.type('checkpoint from pack-editor-check');
    await sleep(200);
    const pressed =
      (await clickByText(page, 'button', 'Commit and build')) ||
      (await clickByText(page, 'button', 'Build'));
    if (!pressed) throw new Error('neither "Commit and build" nor "Build" is on the Build tab');

    const result = await awaitBuild(page);
    await shoot(page, 'check-build');
    console.log('build status:', result.status);
    for (const line of result.log.slice(-4)) if (line.trim()) console.log('  ', line);
    if (result.status !== 'done') process.exitCode = 1;
  }
} finally {
  await browser.close();
}
