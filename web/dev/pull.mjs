// Copy production's public data into web/.devdata, for `pnpm dev:snapshot`.
//
// Only what the mirror serves to anybody: the catalog, every listed pack's
// card, build and history, its discussions, the pictures they name, and each
// mod page a pack links to, with the mod icons. Nothing that needs an account
// is asked for, and nothing is sent but GET.
//
//   node dev/pull.mjs                 everything, icons included
//   NO_ICONS=1 node dev/pull.mjs      without the mod icons, much quicker
//
// SMRT_PROD overrides the mirror read from.

import { createHash } from 'node:crypto';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const PROD = (process.env.SMRT_PROD ?? 'https://smrt.hivens.dev').replace(/\/+$/, '');
const OUT = join(dirname(fileURLToPath(import.meta.url)), '..', '.devdata');
const ICONS = !process.env.NO_ICONS;
const AT_ONCE = 6;

// The same rule as snapshotKey in devdata.ts, which looks these up.
function keyOf(rawUrl) {
  const u = new URL(rawUrl, 'http://dev.invalid');
  return decodeURIComponent(u.pathname) + u.search;
}

rmSync(OUT, { recursive: true, force: true });
mkdirSync(join(OUT, 'files'), { recursive: true });

const index = {};
let bytes = 0;
let failed = 0;

/// Fetch one public path and keep the answer, whatever it was: a 404 kept is
/// a 404 the dev server can give back without asking anybody.
async function take(path) {
  const key = keyOf(path);
  if (index[key]) return index[key].json;
  let r;
  try {
    r = await fetch(`${PROD}${path}`, { headers: { 'user-agent': 'smrt-devdata' } });
  } catch (e) {
    failed += 1;
    console.warn(`  ! ${path}: ${e.message}`);
    return null;
  }
  const type = r.headers.get('content-type') ?? 'application/octet-stream';
  let body = Buffer.from(await r.arrayBuffer());
  let json = null;
  if (type.includes('application/json')) {
    // what production addresses as itself is addressed from here instead
    const text = body.toString('utf8').replaceAll(PROD, '');
    body = Buffer.from(text, 'utf8');
    try {
      json = JSON.parse(text);
    } catch {
      json = null;
    }
  }
  const file = createHash('sha1').update(key).digest('hex');
  writeFileSync(join(OUT, 'files', file), body);
  index[key] = { file, status: r.status, type, json };
  bytes += body.length;
  return json;
}

async function each(items, fn) {
  let i = 0;
  await Promise.all(
    Array.from({ length: AT_ONCE }, async () => {
      while (i < items.length) await fn(items[i++]);
    }),
  );
}

const enc = encodeURIComponent;

console.log(`pulling public data from ${PROD}`);
for (const p of ['/v1/health', '/v1/featured', '/v1/servers', '/v1/cache/inventory']) await take(p);
const official = (await take('/v1/packs'))?.packs ?? [];
const community = ((await take('/v1/community')) ?? []).map((c) => c.summary);
const packs = [...official, ...community];
console.log(`  ${packs.length} packs`);

const mods = new Map();
const threads = new Set();
const pictures = new Set();
await each(packs, async (p) => {
  const base = `/v1/packs/${enc(p.pack_id)}`;
  const summary = await take(base);
  for (const u of [summary?.banner_url, summary?.icon_url, ...(summary?.gallery_urls ?? [])]) {
    if (u && u.startsWith('/v1/')) pictures.add(u);
  }
  const manifest = await take(`${base}/manifest`);
  await take(`${base}/manifest/versions`);
  for (const q of ['', '?all=true']) {
    const rows = await take(`${base}/threads${q}`);
    for (const t of Array.isArray(rows) ? rows : []) threads.add(t.id);
  }
  for (const m of manifest?.mods ?? []) mods.set(m.sha1, m);
});
await each([...pictures], take);
await each([...threads], (id) => take(`/v1/threads/${id}`));
console.log(`  ${pictures.size} pictures, ${threads.size} discussions`);

// each mod page a pack links to, and the pages one step away from them
const related = new Set();
await each([...mods.keys()], async (sha1) => {
  const detail = await take(`/v1/mods/${enc(`sha1:${sha1}`)}`);
  for (const e of detail?.edges ?? []) if (e.other_mod_id != null) related.add(e.other_mod_id);
});
await each([...related], (id) => take(`/v1/mods/${id}`));
console.log(`  ${mods.size} mods, ${related.size} related mod pages`);

if (ICONS) {
  const icons = new Set();
  for (const [sha1, m] of mods) {
    icons.add(`/v1/cache/icon/${sha1}`);
    if (m.source?.type === 'modrinth' && m.source.project_id) icons.add(`/v1/modrinth/icon/${enc(m.source.project_id)}`);
  }
  await each([...icons], take);
  console.log(`  ${icons.size} icons asked for`);
}

const stored = Object.fromEntries(
  Object.entries(index).map(([k, v]) => [k, { file: v.file, status: v.status, type: v.type }]),
);
writeFileSync(join(OUT, 'index.json'), JSON.stringify(stored));
console.log(
  `done: ${Object.keys(stored).length} responses, ${(bytes / 1e6).toFixed(1)} MB in ${OUT}` +
    (failed ? `, ${failed} could not be fetched` : ''),
);
