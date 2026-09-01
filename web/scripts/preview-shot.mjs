// Seed a pack with something worth looking at, publish it, stage a change, then
// open the editor's Preview and screenshot the launcher-faithful render.
//
// The seeding half used to call `/v1/admin/...` and a `curator` endpoint, and to
// hand-set `required` on config rows. None of those exist: the admin paths
// became `/v1/authoring/...`, the curator TOML is gone, and required-ness is
// derived at build time rather than declared. Rewritten against the API as it
// is; nothing off this mirror is needed, since every source is a cache jar.
//
// Env: the usual (see scripts/lib/harness.mjs), plus PACK (default `Preview`).
import { createHash } from 'node:crypto';
import { launch, signedIn, editorTab, clickByText, shoot, sleep, BASE, SESSION }
  from './lib/harness.mjs';

const PACK = process.env.PACK ?? 'Preview';
const sha1 = (s) => createHash('sha1').update(s).digest('hex');

if (!SESSION) {
  console.error('SESSION is required: the seeding half writes through the authoring API.');
  process.exit(1);
}

async function api(method, path, body, contentType = 'application/json') {
  const isJson = contentType === 'application/json';
  const r = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      Cookie: `smrt_session=${SESSION}`,
      ...(body === undefined ? {} : { 'Content-Type': contentType }),
    },
    body: body === undefined ? undefined : isJson ? JSON.stringify(body) : body,
  });
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${await r.text()}`);
  return r.status === 204 ? null : r.json().catch(() => null);
}

/// A one-entry stored (uncompressed) zip, built by hand so this script needs no
/// dependency of its own. Real zip bytes, so the harvest reads an identity out
/// of the jar rather than skipping it as unreadable.
function zipOf(name, content) {
  const nameBytes = Buffer.from(name, 'utf8');
  const data = Buffer.from(content, 'utf8');
  const table = [...Array(256)].map((_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  let crc = 0xffffffff;
  for (const b of data) crc = table[(crc ^ b) & 0xff] ^ (crc >>> 8);
  crc = (crc ^ 0xffffffff) >>> 0;

  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  local.writeUInt16LE(20, 4);
  local.writeUInt16LE(0, 8); // stored
  local.writeUInt32LE(crc, 14);
  local.writeUInt32LE(data.length, 18);
  local.writeUInt32LE(data.length, 22);
  local.writeUInt16LE(nameBytes.length, 26);

  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0);
  central.writeUInt16LE(20, 4);
  central.writeUInt16LE(20, 6);
  central.writeUInt16LE(0, 10);
  central.writeUInt32LE(crc, 16);
  central.writeUInt32LE(data.length, 20);
  central.writeUInt32LE(data.length, 24);
  central.writeUInt16LE(nameBytes.length, 28);

  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(1, 8);
  end.writeUInt16LE(1, 10);
  end.writeUInt32LE(46 + nameBytes.length, 12);
  end.writeUInt32LE(30 + nameBytes.length + data.length, 16);

  return Buffer.concat([local, nameBytes, data, central, nameBytes, end]);
}

async function putJar(modid, name, version) {
  const body = zipOf(
    'mcmod.info',
    JSON.stringify([{ modid, name, version, description: `${name}, for the preview.` }]),
  );
  const hash = sha1(body);
  await api('PUT', `/v1/cache/${hash.slice(0, 2)}/${hash}.jar`, body, 'application/java-archive');
  return hash;
}

const mod = (filename, hash, display) => ({
  filename,
  default_enabled: true,
  source: { type: 'smrt_cache', sha1: hash },
  ...(display ? { display } : {}),
  pulled: false,
});

const config = (mods, note) => ({
  pack_id: PACK,
  display_name: 'Preview',
  tagline: 'What a build looks like before it is one',
  minecraft_version: '1.12.2',
  loader: { name: 'forge', version: '14.23.5.2860' },
  java_major: 8,
  version: '0.1',
  tags: ['demo'],
  featured: false,
  mods,
  assets: [],
  pack_meta: {
    icon_url: null,
    banner_url: null,
    gallery_urls: [],
    description_md: `# Preview\n\n${note}\n\n- a list item\n- and another`,
  },
  owner: 0,
  tier: 'official',
  visibility: 'published',
});

async function buildAndWait(message) {
  await api('POST', `/v1/authoring/packs/${PACK}/commits`, { message });
  const { job_id } = await api('POST', `/v1/authoring/packs/${PACK}/build`);
  for (let i = 0; i < 700; i++) {
    const s = await api('GET', `/v1/jobs/${job_id}`);
    if (s.status !== 'running') {
      if (s.status !== 'done') throw new Error(`build failed:\n  ${s.log.slice(-3).join('\n  ')}`);
      return s;
    }
    await sleep(500);
  }
  throw new Error('build timeout');
}

// ── seed ────────────────────────────────────────────────────────────────────
const jei = await putJar('jei', 'Just Enough Items', '4.16.1');
const lib = await putJar('codechickenlib', 'CodeChicken Lib', '3.2.3');
const addon = await putJar('jeiaddon', 'JEI Addon', '1.4.0');
console.log('seeded three jars');

await api(
  'PUT',
  `/v1/authoring/packs/${PACK}/config`,
  config([mod('jei-4.16.1.jar', jei), mod('codechickenlib-3.2.3.jar', lib)], 'The published build.'),
);
await buildAndWait('the published build');
console.log('published v1');

// stage a change, so the preview has something to differ from
await api(
  'PUT',
  `/v1/authoring/packs/${PACK}/config`,
  config(
    [
      mod('jei-4.16.1.jar', jei),
      mod('codechickenlib-3.2.3.jar', lib),
      mod('jeiaddon-1.4.0.jar', addon, {
        requires: [{ filename: 'jei-4.16.1.jar', optional: false }],
      }),
    ],
    'One mod more than what is published.',
  ),
);
console.log('staged v2');

// ── shoot ───────────────────────────────────────────────────────────────────
const browser = await launch({ width: 1280, height: 1000 });
try {
  const page = await signedIn(browser);
  await page.goto(`${BASE}/packs/${encodeURIComponent(PACK)}`, { waitUntil: 'networkidle0' });
  await page.waitForSelector('[role=tab]', { timeout: 8000 });
  await editorTab(page, 'Config');
  await sleep(400);
  if (!(await clickByText(page, 'button', 'Preview'))) throw new Error('no Preview button');
  // the preview is a dry run: resolve, classify, then render what a launcher
  // would show. It waits out a pending harvest first, so give it room.
  await page.waitForSelector('.hero, .jl .st.bad', { timeout: 400_000 });
  await sleep(1500);
  await shoot(page, 'smrt-preview');
  const rows = await page.$$eval('.mrow, .modrow, .prow', (els) => els.length);
  console.log(`preview rendered with ${rows} row(s)`);
} finally {
  await browser.close();
}
