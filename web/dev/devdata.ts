// Where the panel's dev server gets its data, and who it says you are.
//
// Three sources, picked by SMRT_DATA:
//
//   local     the default: /v1 goes to a mirror on this machine
//   live      /v1 goes to production, read only: every write is refused here,
//             before it leaves, and the browser's cookies are never sent on
//   snapshot  /v1 is answered from web/.devdata, a copy of production's public
//             data taken by `pnpm devdata:pull`; no network at all
//
// DEV_AS=member|admin|debug skips signing in for the two production modes:
// the dev server answers "who am I" itself, and what a signed-in account would
// read and the public data does not hold comes back empty rather than as a
// 401, which the panel would take for an expired session and sign out on.
// DEV_SESSION=<smrt_session> instead reads production as that account, still
// read only.
//
// None of this is in the built panel or in the mirror: it exists only while
// `vite` serves the source.

import type { Plugin, ProxyOptions } from 'vite';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

export type DataMode = 'local' | 'live' | 'snapshot';
const MODES: DataMode[] = ['local', 'live', 'snapshot'];
const ROLES = ['member', 'admin', 'debug'] as const;
type Role = (typeof ROLES)[number];

export interface DevDataOptions {
  mode: DataMode;
  /// production's origin, for live and for rewriting what a snapshot holds
  prod: string;
  /// the local mirror, for mode local
  local: string;
  /// DEV_AS: answer "who am I" with this role instead of signing in
  as?: string;
  /// DEV_SESSION: read production as this session
  session?: string;
  /// web/, where .devdata lives
  root: string;
}

/// Read and check the environment once, so a typo stops the server with a
/// sentence instead of quietly running in another mode.
export function devOptions(env: NodeJS.ProcessEnv, root: string): DevDataOptions {
  const mode = (env.SMRT_DATA ?? 'local') as DataMode;
  if (!MODES.includes(mode)) {
    throw new Error(`SMRT_DATA=${mode}: expected one of ${MODES.join(', ')}`);
  }
  const as = env.DEV_AS?.trim() || undefined;
  if (as && as !== 'guest' && !ROLES.includes(as as Role)) {
    throw new Error(`DEV_AS=${as}: expected guest, ${ROLES.join(', ')}`);
  }
  return {
    mode,
    prod: (env.SMRT_PROD ?? 'https://smrt.hivens.dev').replace(/\/+$/, ''),
    local: env.SMRT_LOCAL ?? 'http://127.0.0.1:9000',
    as: as === 'guest' ? undefined : as,
    session: env.DEV_SESSION?.trim() || undefined,
    root,
  };
}

function sendJson(res: ServerResponse, status: number, body: unknown) {
  res.statusCode = status;
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(body));
}

function refuse(res: ServerResponse, status: number, code: string, message: string) {
  sendJson(res, status, { schema_version: 2, error: { code, message } });
}

/// An event stream that stays open and says nothing. The panel subscribes to
/// the mirror's changes, and to a pack's presence while its editor is open;
/// against production that subscription would announce a dev session as an
/// editor in the room, and a refused one would reconnect in a loop.
function quietStream(res: ServerResponse) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    Connection: 'keep-alive',
  });
  res.write(': dev server, no events\n\n');
}

/// Who the dev server says you are, and the account's own lists, which hold
/// nothing for an account that does not exist.
function identity(req: IncomingMessage, res: ServerResponse, role: Role, path: string): boolean {
  if (path === '/v1/me') {
    sendJson(res, 200, { uid: 1, login: `dev-${role}`, role, accepted_terms: true });
    return true;
  }
  if (!path.startsWith('/v1/me/')) return false;
  if (req.method !== 'GET') return false;
  if (path === '/v1/me/notifications') sendJson(res, 200, { unread: 0, rows: [] });
  else if (path === '/v1/me/packs' || path === '/v1/me/authoring' || path === '/v1/me/uploads')
    sendJson(res, 200, []);
  else refuse(res, 404, 'dev_no_account', 'the dev account has none of this');
  return true;
}

// ── snapshot ──

type SnapEntry = { file: string; status: number; type: string };

function loadSnapshot(root: string): Record<string, SnapEntry> | null {
  const index = join(root, '.devdata', 'index.json');
  if (!existsSync(index)) return null;
  return JSON.parse(readFileSync(index, 'utf8')) as Record<string, SnapEntry>;
}

/// The key a snapshot stores a response under: the decoded path, with the
/// query when there is one. Shared with the pull script.
export function snapshotKey(rawUrl: string): { exact: string; path: string } {
  const u = new URL(rawUrl, 'http://dev.invalid');
  const path = decodeURIComponent(u.pathname);
  return { exact: path + u.search, path };
}

// ── the plugin ──

export function devData(o: DevDataOptions): Plugin {
  const role = (o.mode !== 'local' && !o.session ? o.as : undefined) as Role | undefined;
  const snapshot = o.mode === 'snapshot' ? loadSnapshot(o.root) : null;

  return {
    name: 'smrt-dev-data',
    apply: 'serve',
    configureServer(server) {
      const say = server.config.logger.info;
      if (o.mode === 'local') say(`  smrt data: local mirror at ${o.local}`);
      if (o.mode === 'live') say(`  smrt data: ${o.prod}, read only`);
      if (o.mode === 'snapshot')
        say(
          snapshot
            ? `  smrt data: snapshot in .devdata (${Object.keys(snapshot).length} responses)`
            : '  smrt data: no snapshot in .devdata yet, run pnpm devdata:pull',
        );
      if (o.session && o.mode === 'live') say('  smrt identity: the DEV_SESSION account');
      else if (role) say(`  smrt identity: dev-${role}, signed in without a login`);
      if (o.as && o.mode === 'local')
        say('  DEV_AS has no effect on a local mirror: sign in there, see docs/development.md');

      server.middlewares.use((req, res, next) => {
        const url = req.url ?? '';
        if (!url.startsWith('/v1')) return next();
        if (o.mode === 'local') return next();
        const { exact, path } = snapshotKey(url);

        // nothing is written anywhere from here
        if (!['GET', 'HEAD', 'OPTIONS'].includes(req.method ?? 'GET')) {
          return refuse(res, 405, 'dev_read_only', `${o.mode} data is read only: nothing was sent`);
        }
        if (path.endsWith('/events')) return quietStream(res);
        if (role && identity(req, res, role, path)) return;

        if (o.mode === 'snapshot') {
          const hit = snapshot?.[exact] ?? snapshot?.[path];
          if (!hit) return refuse(res, 404, 'dev_not_in_snapshot', `not in the snapshot: ${exact}`);
          res.statusCode = hit.status;
          res.setHeader('Content-Type', hit.type);
          return res.end(readFileSync(join(o.root, '.devdata', 'files', hit.file)));
        }
        next();
      });
    },
  };
}

/// The proxy for the mode: the local mirror, production, or none when the
/// snapshot answers everything itself.
export function devProxy(o: DevDataOptions): Record<string, string | ProxyOptions> | undefined {
  if (o.mode === 'local') return { '/v1': o.local };
  if (o.mode === 'snapshot') return undefined;
  const signedOut = !o.session;
  return {
    '/v1': {
      target: o.prod,
      changeOrigin: true,
      selfHandleResponse: true,
      configure(proxy) {
        proxy.on('proxyReq', (proxyReq) => {
          // the browser's cookies belong to localhost and stay there
          proxyReq.removeHeader('cookie');
          if (o.session) proxyReq.setHeader('cookie', `smrt_session=${o.session}`);
          // plain bodies, so the JSON below can be read and rewritten
          proxyReq.setHeader('accept-encoding', 'identity');
        });
        proxy.on('proxyRes', (proxyRes, _req, res) => {
          let status = proxyRes.statusCode ?? 502;
          const type = String(proxyRes.headers['content-type'] ?? '');
          const headers = { ...proxyRes.headers };
          delete headers['set-cookie'];
          delete headers['content-length'];
          // Signed out, a 401 is production saying this needs an account. The
          // panel would read it as a lapsed session and sign the dev account
          // out, so it arrives as the plain absence it is.
          if (signedOut && (status === 401 || status === 403)) {
            proxyRes.resume();
            return refuse(res, 404, 'dev_needs_session', 'needs a signed-in account: set DEV_SESSION');
          }
          if (!type.includes('application/json')) {
            res.writeHead(status, headers);
            proxyRes.pipe(res);
            return;
          }
          // Addresses production writes into its answers (banners, icons)
          // point back at itself; made relative, the page fetches them through
          // this server, from its own origin, as it does when it is deployed.
          const chunks: Buffer[] = [];
          proxyRes.on('data', (c: Buffer) => chunks.push(c));
          proxyRes.on('end', () => {
            const body = Buffer.concat(chunks).toString('utf8').replaceAll(o.prod, '');
            status = proxyRes.statusCode ?? status;
            res.writeHead(status, { ...headers, 'content-length': Buffer.byteLength(body) });
            res.end(body);
          });
        });
      },
    },
  };
}
