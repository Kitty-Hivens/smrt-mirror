// The panel's logic that fails silently, checked here rather than in a
// browser: the merge (#115), and what Java a pack needs (#126).
//
// The merge half:
//
// The editor keeps plain `bind:value` controls over a plain config object, so
// every keystroke arrives here as a whole new config. Turning that into the
// smallest edit is what decides whether two people typing in one paragraph
// merge or overwrite each other -- assigning a whole string to a text node is a
// delete of everything followed by an insert of everything, and the other
// person's sentence is inside the range being deleted.
//
// Plain node, no framework: the panel has no test runner, and this is one file
// of pure functions. `node web/scripts/merge-check.mjs`, or `pnpm merge-check`.
import * as Y from 'yjs';
import { readConfig, textPatch, writeConfig } from '../src/lib/packdoc.ts';
import { JAVA_MAJORS, suggestedJava } from '../src/lib/java.ts';
import { changedPaths } from '../src/lib/touched.svelte.ts';
import { advertisesModList } from '../src/lib/handshake.ts';
import { assetPath, isPackFile, ASSET_PREFIX } from '../src/lib/packassets.ts';
import { nextPageUrl } from '../src/lib/pagelink.ts';
import { suggest, tally } from '../src/lib/changes.ts';
import { renderMarkdown, safeUrl } from '../src/lib/markdown.ts';
import { diffManifests } from '../src/lib/diff.ts';
import { resolve } from '../src/lib/message.ts';
import { en } from '../src/lib/locales/en.ts';
import { ru } from '../src/lib/locales/ru.ts';

let failures = 0;
const say = (dict, loc, key, n) => resolve(dict[key], loc, { n, count: n });
const check = (name, cond, detail = '') => {
  if (cond) console.log(`ok   ${name}`);
  else {
    failures++;
    console.log(`FAIL ${name} ${detail}`);
  }
};

const base = {
  pack_id: 'Industrial', display_name: 'Industrial', tagline: 'Heavy tech',
  minecraft_version: '1.12.2', loader: { name: 'forge', version: '14.23.5.2860' },
  java_major: 8, version: '0.4', tags: ['tech'], featured: true,
  mods: [{ filename: 'jei.jar', default_enabled: true, source: { type: 'smrt_cache', sha1: 'a'.repeat(40) }, pulled: false }],
  assets: [], pack_meta: { description_md: 'A pack.', gallery_urls: [] },
  owner: 211033194, tier: 'community', visibility: 'draft', fork_of: 'Create',
};

// ── the patch itself ────────────────────────────────────────────────────────
check('append is a patch at the end',
  JSON.stringify(textPatch('abc', 'abcd')) === JSON.stringify({ index: 3, remove: 0, insert: 'd' }));
check('backspace removes one at a point',
  JSON.stringify(textPatch('abcd', 'abd')) === JSON.stringify({ index: 2, remove: 1, insert: '' }));
check('typing in the middle inserts there',
  JSON.stringify(textPatch('ad', 'abcd')) === JSON.stringify({ index: 1, remove: 0, insert: 'bc' }));
check('no change is no patch', textPatch('same', 'same') === null);
{
  // property check: applying the patch must reproduce the target, always
  const words = ['', 'a', 'ab', 'abc', 'hello world', 'hello brave world', 'held', 'hello worlds'];
  let ok = true;
  for (const from of words) for (const to of words) {
    const p = textPatch(from, to);
    const out = p === null ? from : from.slice(0, p.index) + p.insert + from.slice(p.index + p.remove);
    if (out !== to) { ok = false; console.log(`  ${JSON.stringify(from)} -> ${JSON.stringify(to)} gave ${JSON.stringify(out)}`); }
  }
  check('every patch reproduces its target', ok);
}

// ── two editors ─────────────────────────────────────────────────────────────
function editor(seed) {
  const doc = new Y.Doc();
  Y.applyUpdate(doc, seed);
  return doc;
}
const server = new Y.Doc();
writeConfig(server, base, 'seed');
const seed = Y.encodeStateAsUpdate(server);

{
  // both typing into the same paragraph, neither having seen the other
  const ada = editor(seed);
  const bo = editor(seed);
  const a = structuredClone(base);
  a.pack_meta.description_md = 'A pack. Heavy tech.';       // appended
  writeConfig(ada, a, 'local');
  const b = structuredClone(base);
  b.pack_meta.description_md = 'The A pack.';               // prefixed
  writeConfig(bo, b, 'local');

  Y.applyUpdate(ada, Y.encodeStateAsUpdate(bo));
  Y.applyUpdate(bo, Y.encodeStateAsUpdate(ada));
  const text = readConfig(ada, base).pack_meta.description_md;
  check('both people keep their words', text.includes('Heavy tech.') && text.startsWith('The '), `got ${JSON.stringify(text)}`);
  check('and the two editors agree', text === readConfig(bo, base).pack_meta.description_md);
}

{
  // both adding a mod, neither having seen the other
  const ada = editor(seed);
  const bo = editor(seed);
  const a = structuredClone(base);
  a.mods.push({ filename: 'ae2.jar', default_enabled: true, source: { type: 'smrt_cache', sha1: 'b'.repeat(40) }, pulled: false });
  writeConfig(ada, a, 'local');
  const b = structuredClone(base);
  b.mods.push({ filename: 'thermal.jar', default_enabled: true, source: { type: 'smrt_cache', sha1: 'c'.repeat(40) }, pulled: false });
  writeConfig(bo, b, 'local');

  Y.applyUpdate(ada, Y.encodeStateAsUpdate(bo));
  const names = readConfig(ada, base).mods.map((m) => m.filename);
  check('both additions land', names.length === 3 && names.includes('ae2.jar') && names.includes('thermal.jar'), JSON.stringify(names));
}

{
  // one person renames a mod while the other edits the prose: different things,
  // no collision, which is the everyday case the old save refused
  const ada = editor(seed);
  const bo = editor(seed);
  const a = structuredClone(base);
  a.mods[0].filename = 'jei-4.16.jar';
  writeConfig(ada, a, 'local');
  const b = structuredClone(base);
  b.tagline = 'Heavy tech, now heavier';
  writeConfig(bo, b, 'local');

  Y.applyUpdate(ada, Y.encodeStateAsUpdate(bo));
  const out = readConfig(ada, base);
  check('a rename and a retagline both survive',
    out.mods[0].filename === 'jei-4.16.jar' && out.tagline === 'Heavy tech, now heavier',
    JSON.stringify([out.mods[0].filename, out.tagline]));
}

{
  // the server's fields never travel, and the editor keeps what it loaded
  const doc = editor(seed);
  const out = readConfig(doc, base);
  check('server fields come from the loaded config',
    out.owner === base.owner && out.visibility === 'draft' && out.fork_of === 'Create');
  check('and are absent from the document itself',
    !['owner', 'tier', 'visibility', 'fork_of'].some((k) => doc.getMap('config').has(k)));
}

{
  // a full round trip, so the mapping is not quietly lossy
  const doc = editor(seed);
  check('a config round-trips', JSON.stringify(readConfig(doc, base)) === JSON.stringify(base),
    JSON.stringify(readConfig(doc, base)));
}

// ── which Java a pack needs ─────────────────────────────────────────────────
// Every one of these is a pack this mirror actually serves.
check('1.12.2 forge wants 8', suggestedJava('1.12.2', 'forge') === 8);
check('1.21.1 neoforge wants 21', suggestedJava('1.21.1', 'neoforge') === 21);
check('1.7.10 on lwjgl3ify wants 21, not 8',
  suggestedJava('1.7.10', 'lwjgl3ify') === 21,
  'the loader exists to run old Minecraft on new Java; deriving from the version alone gets this wrong');
check('cleanroom is the same kind of loader', suggestedJava('1.12.2', 'cleanroom') === 21);
check('1.17 wants 16', suggestedJava('1.17', 'forge') === 16);
check('1.18.2 wants 17', suggestedJava('1.18.2', 'forge') === 17);
check('1.20.4 still wants 17', suggestedJava('1.20.4', 'forge') === 17);
check('1.20.5 moves to 21', suggestedJava('1.20.5', 'forge') === 21);
check('versions compare piecewise, not as strings',
  suggestedJava('1.9.4', 'forge') === 8,
  'lexically "1.9" > "1.18", which is how a naive compare puts 1.9 on Java 17');
check('an unparseable version suggests nothing', suggestedJava('', 'forge') === null && suggestedJava('snapshot', 'forge') === null);
check('the offered list holds what old packs need', [8, 11, 16, 17, 21].every((v) => JAVA_MAJORS.includes(v)));

// ── who changed what ────────────────────────────────────────────────────────
// A marker is only useful if it points at the thing that moved. Reporting the
// whole config, or the whole mods list, is an address nobody can act on.
{
  const base = {
    display_name: 'Industrial',
    pack_meta: { description_md: 'A pack.', gallery_urls: [] },
    mods: [{ filename: 'jei.jar', default_enabled: true }, { filename: 'ae2.jar', default_enabled: true }],
  };
  const clone = () => JSON.parse(JSON.stringify(base));

  const scalar = clone(); scalar.display_name = 'Industrial II';
  check('a changed scalar is reported at its own path',
    JSON.stringify(changedPaths(base, scalar)) === '["display_name"]',
    JSON.stringify(changedPaths(base, scalar)));

  const nested = clone(); nested.pack_meta.description_md = 'A pack. Heavy tech.';
  check('a nested field is reported at its full path',
    JSON.stringify(changedPaths(base, nested)) === '["pack_meta.description_md"]',
    JSON.stringify(changedPaths(base, nested)));

  const row = clone(); row.mods[1].default_enabled = false;
  check('an edited row is reported as the row, not the whole list',
    JSON.stringify(changedPaths(base, row)) === '["mods.ae2.jar"]',
    JSON.stringify(changedPaths(base, row)));

  const added = clone(); added.mods.push({ filename: 'thermal.jar' });
  check('an added row is one path, not every field in it',
    JSON.stringify(changedPaths(base, added)) === '["mods.thermal.jar"]',
    JSON.stringify(changedPaths(base, added)));

  const removed = clone(); removed.mods.pop();
  check('a removed row is reported too',
    JSON.stringify(changedPaths(base, removed)) === '["mods.ae2.jar"]',
    JSON.stringify(changedPaths(base, removed)));

  // a marker keyed by position marks the wrong row as soon as one arrives above
  // it -- and marks every row below as touched by whoever added it
  const inserted = clone(); inserted.mods.splice(1, 0, { filename: 'create.jar' });
  check('a row arriving in the middle touches one row',
    JSON.stringify(changedPaths(base, inserted)) === '["mods.create.jar"]',
    JSON.stringify(changedPaths(base, inserted)));

  const reordered = clone(); reordered.mods.reverse();
  check('reordering the list touches nothing',
    changedPaths(base, reordered).length === 0,
    JSON.stringify(changedPaths(base, reordered)));

  check('an unchanged config reports nothing', changedPaths(base, clone()).length === 0);

  // two rows can carry the same name (nothing enforces otherwise); collapsing
  // them into one would drop whichever edit landed on the loser
  const twins = { mods: [{ filename: 'a.jar', on: true }, { filename: 'a.jar', on: true }] };
  const twinEdit = { mods: [{ filename: 'a.jar', on: false }, { filename: 'a.jar', on: true }] };
  check('rows sharing a name keep their edits',
    JSON.stringify(changedPaths(twins, twinEdit)) === '["mods.0"]',
    JSON.stringify(changedPaths(twins, twinEdit)));

  // the editor adds an asset with an empty dest and lets you fill it in; that
  // one blank row must not put every named row back on positions
  const named2 = { assets: [{ dest: 'a.json' }, { dest: 'b.json' }] };
  const withBlank = { assets: [{ dest: '' }, { dest: 'a.json' }, { dest: 'b.json' }] };
  check('a blank row does not drag the named ones back to positions',
    JSON.stringify(changedPaths(named2, withBlank)) === '["assets.0"]',
    JSON.stringify(changedPaths(named2, withBlank)));

  // plain values have nothing to be named by, so they stay positional
  const tagged = { tags: ['tech', 'magic'] };
  check('a list of plain values is still addressed by position',
    JSON.stringify(changedPaths(tagged, { tags: ['tech', 'quests'] })) === '["tags.1"]',
    JSON.stringify(changedPaths(tagged, { tags: ['tech', 'quests'] })));

  const two = clone();
  two.display_name = 'X'; two.mods[0].default_enabled = false;
  check('two independent changes are two paths',
    JSON.stringify(changedPaths(base, two).sort()) === '["display_name","mods.jei.jar"]',
    JSON.stringify(changedPaths(base, two)));
}

// #148: whether a handshake claim can be derived at all, from the loader alone.
{
  check('a 1.12.2 forge server advertises its mod list', advertisesModList('forge') === true);
  check('so does a fork that inherits one', advertisesModList('cleanroom') === true);
  check('and the modernised 1.7.10 loader', advertisesModList('lwjgl3ify') === true);
  // the case that sends people pressing a button that cannot work
  check('a neoforge server advertises nothing', advertisesModList('neoforge') === false);
  check('nor does fabric', advertisesModList('fabric') === false);
  check('a loader nobody named is not assumed to advertise', advertisesModList('') === false);
  check('the answer does not depend on spelling', advertisesModList('  NeoForge ') === false);
}

// A pack's own files are named for the pack, not for one launcher, and the old
// name keeps resolving for every pack that already uses it.
{
  check('new files are minted under the neutral prefix',
    assetPath('icon.png') === '_pack/icon.png', assetPath('icon.png'));
  check('nested ones too', assetPath('assets', 'servers.dat') === '_pack/assets/servers.dat');
  check('the prefix names no launcher', !/nexira|smrt/i.test(ASSET_PREFIX), ASSET_PREFIX);
  check('it stays out of the way of game directories', ASSET_PREFIX.startsWith('_'));

  // the sweep that keeps an icon resolving to one file has to see both names,
  // or a re-upload leaves the old image behind under the old prefix
  check('an icon is recognised under the new prefix', isPackFile('_pack/icon.png', 'icon'));
  check('and under the old one', isPackFile('_nexira/icon.webp', 'icon'));
  check('a banner is not an icon', !isPackFile('_pack/banner.png', 'icon'));
  check('and a lookalike elsewhere is neither',
    !isPackFile('resourcepacks/icon.png', 'icon'));
}

// Walking a paged listing means following the address the mirror hands back. Read
// it wrong and the walk stops at the first page while looking like it finished.
{
  const link = '</v1/audit?limit=60&after=NDI>; rel="next"';
  check('the next page is the address it names',
    nextPageUrl(link) === '/v1/audit?limit=60&after=NDI', nextPageUrl(link));
  check('the last page names nothing after it', nextPageUrl(null) === null);
  check('a header with no next in it yields none',
    nextPageUrl('</v1/audit?limit=60>; rel="prev"') === null);
  check('the next is found among other relations',
    nextPageUrl('</a>; rel="prev", </b>; rel="next"') === '/b');
  // the cursor is base64url, so it can carry characters a naive split would eat
  check('a cursor is taken whole',
    nextPageUrl('</v1/registry/mods?q=a-b_c&after=eyJhIjoxfQ>; rel="next"')
      === '/v1/registry/mods?q=a-b_c&after=eyJhIjoxfQ');
}

// ── reading a list of changes ───────────────────────────────────────────────
// The diff itself moved to the mirror (`src/authoring/configdiff.rs`), which is
// what made the count beside the list and the list agree. What stays here is
// what the panel does with the rows: sum them, and offer a first line for the
// message box, so nobody writes "misc" over four mods they no longer remember.
{
  const row = (op, label, group = 'mods') => ({ group, op, label, key: `f:${label}` });

  check('nothing changed suggests nothing', suggest([]) === null);

  check('one arrival is named',
    JSON.stringify(suggest([row('add', 'Cosmetica.jar')])) ===
      JSON.stringify({ kind: 'add', what: ['Cosmetica.jar'], counts: { add: 1, remove: 0, change: 0 } }),
    JSON.stringify(suggest([row('add', 'Cosmetica.jar')])));

  const three = ['a.jar', 'b.jar', 'c.jar'].map((f) => row('add', f));
  check('three are still named', suggest(three).kind === 'add' && suggest(three).what.length === 3);

  const four = ['a.jar', 'b.jar', 'c.jar', 'd.jar'].map((f) => row('add', f));
  check('four are counted, not listed',
    suggest(four).kind === 'mixed' && suggest(four).counts.add === 4,
    JSON.stringify(suggest(four)));

  check('arrivals and departures together are counted',
    suggest([row('add', 'a.jar'), row('remove', 'b.jar')]).kind === 'mixed');

  // a re-pin and a toggle on one mod are two rows and one name: the message
  // should read "Update sodium.jar", not "Update sodium.jar, sodium.jar"
  const twice = [
    { ...row('change', 'sodium.jar'), field: 'pin' },
    { ...row('change', 'sodium.jar'), field: 'default_enabled' },
  ];
  check('one file changed twice is one name',
    suggest(twice).kind === 'update' && suggest(twice).what.join() === 'sodium.jar',
    JSON.stringify(suggest(twice)));

  check('the tally counts each operation',
    JSON.stringify(tally([row('add', 'a.jar'), row('add', 'b.jar'), row('remove', 'c.jar')])) ===
      JSON.stringify({ add: 2, remove: 1, change: 0 }));
}

// ── the description renderer ────────────────────────────────────────────────
//
// It renders text somebody else wrote -- a community pack's description -- into
// a page other people read, so what it must never do is emit markup or a link
// its author did not write.
{
  const md = (s) => renderMarkdown(s);

  check('raw html is text, not markup',
    md('<img src=x onerror=alert(1)>').includes('&lt;img') &&
    !md('<img src=x onerror=alert(1)>').includes('<img'));
  check('a script url is not a link target',
    md('[x](javascript:alert(1))').includes('href="#"'),
    md('[x](javascript:alert(1))'));
  check('quotes cannot escape an attribute',
    !md('[x](https://a" onmouseover="alert(1))').includes('onmouseover="alert'),
    md('[x](https://a" onmouseover="alert(1))'));
  check('safeUrl refuses the schemes that execute',
    safeUrl('javascript:alert(1)') === '#' && safeUrl('data:text/html,x') === '#' &&
    safeUrl('https://ok/') === 'https://ok/');

  // The renderer parks each finished link, image and code span behind a
  // placeholder while it escapes the prose around them, then puts them back.
  // When that placeholder was printable, prose containing one was substituted
  // for somebody else's span -- so a description reading `@@MD0@@` rendered a
  // second copy of the first link in it.
  const forged = md('@@MD0@@ and [y](https://ok/)');
  check('a placeholder written in the prose stays prose',
    forged.includes('@@MD0@@') && (forged.match(/<a /g) ?? []).length === 1,
    forged);
  check('and the spans it protects still come back',
    md('`code` and [y](https://ok/) and ![i](https://ok/i.png)').includes('<code>code</code>'),
    md('`code` and [y](https://ok/)'));
}

// ── the preview diff ────────────────────────────────────────────────────────
//
// The preview answers "what would publishing change?" locally, because a dry run
// has no published version for the mirror to diff against. It has to give the
// answer the mirror gives, which means matching mods the way `domain/diff.rs`
// does: the Modrinth project, else the curator slug, else the filename.
{
  const manifest = (mods) => ({ pack_version: 'v', mods, assets: [] });
  const mod = (filename, sha1, extra = {}) => ({
    filename, sha1, size_bytes: 1, required: false, default_enabled: true,
    source: { type: 'smrt_cache', url: 'u' }, ...extra,
  });
  const pinned = (filename, sha1, project) => ({
    filename, sha1, size_bytes: 1, required: false, default_enabled: true,
    source: { type: 'modrinth', project_id: project, version_id: 'v' },
  });

  // A re-pin that renames the jar is one mod moving, not one leaving and
  // another arriving -- which is what the update dialog will call it.
  const repin = diffManifests(
    manifest([pinned('sodium-0.5.jar', 's1', 'AANobbMI')]),
    manifest([pinned('sodium-0.6.jar', 's2', 'AANobbMI')]),
  );
  check('a renamed re-pin is an update, not a swap',
    repin.changed.length === 1 && !repin.added.length && !repin.removed.length,
    JSON.stringify(repin));

  // A self-hosted jar carries its version in its name, so the curator slug is
  // what makes it the same mod across builds (ADR 0002).
  const slugged = diffManifests(
    manifest([mod('mymod-1.0.jar', 'a1', { slug: 'mymod' })]),
    manifest([mod('mymod-1.1.jar', 'a2', { slug: 'mymod' })]),
  );
  check('a slugged self-hosted jar is followed across a rename',
    slugged.changed.length === 1 && !slugged.added.length && !slugged.removed.length,
    JSON.stringify(slugged));

  // Two different mods stay two different mods.
  const swap = diffManifests(
    manifest([mod('a.jar', 'a1')]),
    manifest([mod('b.jar', 'b1')]),
  );
  check('an actual swap is still an add and a remove',
    swap.added.length === 1 && swap.removed.length === 1 && !swap.changed.length,
    JSON.stringify(swap));

  const same = diffManifests(manifest([mod('a.jar', 'a1')]), manifest([mod('a.jar', 'a1')]));
  check('an unchanged pack reports nothing', same.unchanged === 1 && !same.changed.length);
}

// tail

// ── counted strings ─────────────────────────────────────────────────────────
// A count and a noun beside it is the one place a dictionary of flat strings
// cannot be right in both languages at once. English needs two forms, Russian
// three, and the wrong one reads as broken rather than as a translation that
// could be better: "1 модов" is not a near miss.
{
  const rus = (key, n) => say(ru, 'ru', key, n);
  const eng = (key, n) => say(en, 'en', key, n);

  check('one mod is a mod', eng('prev.modsChip', 1) === '1 mod', eng('prev.modsChip', 1));
  check('two are mods', eng('prev.modsChip', 2) === '2 mods', eng('prev.modsChip', 2));
  check('none are mods', eng('prev.modsChip', 0) === '0 mods', eng('prev.modsChip', 0));

  // Russian counts in three: 1, then 2-4, then 5 and up, and it starts over at
  // 21. The teens are the trap, 11 goes with the many form and not with 1.
  check('один мод', rus('prev.modsChip', 1) === '1 мод', rus('prev.modsChip', 1));
  check('два мода', rus('prev.modsChip', 2) === '2 мода', rus('prev.modsChip', 2));
  check('пять модов', rus('prev.modsChip', 5) === '5 модов', rus('prev.modsChip', 5));
  check('одиннадцать модов, не мод', rus('prev.modsChip', 11) === '11 модов', rus('prev.modsChip', 11));
  check('двадцать один мод', rus('prev.modsChip', 21) === '21 мод', rus('prev.modsChip', 21));
  check('двадцать два мода', rus('prev.modsChip', 22) === '22 мода', rus('prev.modsChip', 22));
  check('сто одиннадцать модов', rus('prev.modsChip', 111) === '111 модов', rus('prev.modsChip', 111));
  check('ноль модов', rus('prev.modsChip', 0) === '0 модов', rus('prev.modsChip', 0));

  check('один человек', rus('pe.touchedByN', 1) === '1 человек', rus('pe.touchedByN', 1));
  check('два человека', rus('pe.touchedByN', 2) === '2 человека', rus('pe.touchedByN', 2));
  check('пять человек', rus('pe.touchedByN', 5) === '5 человек', rus('pe.touchedByN', 5));
  check('one person, not people', eng('pe.touchedByN', 1) === '1 person', eng('pe.touchedByN', 1));

  check('один ассет', rus('prev.assetsChip', 1) === '1 ассет', rus('prev.assetsChip', 1));
  check('один лишний', rus('pe.valExtra', 1) === '1 лишний', rus('pe.valExtra', 1));
  check('Архив: 1 мод', rus('pe.valArchiveMods', 1) === 'Архив: 1 мод', rus('pe.valArchiveMods', 1));

  // The crutch these replaced: the dictionary used to say "comment(s)".
  check('one comment', eng('thr.comments', 1) === '1 comment', eng('thr.comments', 1));
  check('two comments', eng('thr.comments', 2) === '2 comments', eng('thr.comments', 2));
  check('no bracketed plural survives',
    !JSON.stringify(en).includes('(s)'), 'a "(s)" is still in the English dictionary');

  // `count` is the older placeholder name, still carried by a few keys.
  check('count drives the choice too', eng('cache.count', 1).startsWith('1 jar ·'), eng('cache.count', 1));

  // A sentence naming three counts cannot be counted on one of them, so the
  // three nouns are counted separately and dropped in finished.
  const words = (c) => ({
    add: rus('chg.nArrivals', c.add),
    remove: rus('chg.nDepartures', c.remove),
    change: rus('chg.nChanges', c.change),
  });
  check('одно прибытие, два ухода, пять изменений',
    resolve(ru['commit.leadLive'], 'ru', words({ add: 1, remove: 2, change: 5 })) ===
      'Возврат запишет: 1 прибытие, 2 ухода, 5 изменений.',
    resolve(ru['commit.leadLive'], 'ru', words({ add: 1, remove: 2, change: 5 })));
}


console.log(failures ? `\n${failures} failed` : '\nall good');
process.exit(failures ? 1 : 0);