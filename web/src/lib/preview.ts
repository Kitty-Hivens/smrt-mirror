// TS replicas of the launcher's read-only pack-rendering logic, so the panel
// preview groups, resolves, and labels a manifest byte-identically to what the
// player will see. Mirrors hivens.launcher.smrt.{ModRoleGrouper, DepGraphResolver,
// ModIconResolver} + content-tab section ordering. Pure functions, no I/O.

import type { AssetEntry, Display, ModEntry, PackManifest } from './types';

// ── ModIconResolver: letter-avatar fallback ─────────────────────────────────

// JVM String.hashCode(): 32-bit, h = 31*h + c. Math.imul keeps the multiply
// 32-bit; `| 0` truncates each step. Filenames are ASCII so UTF-16 code units
// match Java chars exactly -- the avatar colour is identical to the launcher's.
function javaHashCode(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (Math.imul(31, h) + s.charCodeAt(i)) | 0;
  return h;
}

function floorMod(a: number, n: number): number {
  return ((a % n) + n) % n;
}

// Matches AVATAR_PALETTE in ModIconImage.kt, in order.
const AVATAR_PALETTE = [
  '#2563eb', '#7c3aed', '#16a34a', '#ca8a04',
  '#dc2626', '#0891b2', '#db2777', '#6b7280',
];

export interface Avatar {
  initials: string;
  color: string;
}

/** Deterministic letter avatar for a mod/asset whose icon could not resolve. */
export function letterAvatar(name: string): Avatar {
  const base = name.replace(/\.jar$/i, '');
  const words = base.split(/[ \-_.]+/).filter(Boolean).slice(0, 2);
  const initials = words.map((w) => w.charAt(0).toUpperCase()).join('') || '?';
  const color = AVATAR_PALETTE[floorMod(javaHashCode(name), AVATAR_PALETTE.length)];
  return { initials, color };
}

/// Initials for the panel's monochrome tile, read off words rather than off
/// whatever the name starts with. A filename stands in for a name often enough
/// that "+Fugue-0.23.7" came out as "+0" and "FontFix-1.12.2" as "F1": a word
/// counts only when it starts with a letter, so prefixes and version numbers
/// drop out, and the second one only when it is letters throughout, so "v3"
/// and "mc1" do not pass for words. The coloured tile keeps the launcher's
/// rule, because the preview is there to show what the launcher draws.
export function readableInitials(name: string): string {
  const words = name.replace(/\.jar$/i, '').split(/[^\p{L}\p{N}]+/u);
  const first = words.findIndex((w) => /^\p{L}/u.test(w));
  if (first < 0) return '?';
  const second = words.slice(first + 1).find((w) => /^\p{L}+$/u.test(w));
  return (words[first].charAt(0) + (second?.charAt(0) ?? '')).toUpperCase();
}

// ── DepGraphResolver ─────────────────────────────────────────────────────────

export interface DepEdge {
  from: string;
  to: string;
  versionRange: string | null;
  optional: boolean;
}

export interface MissingReq {
  from: string;
  requires: string;
}

export interface DepGraph {
  edges: DepEdge[];
  missing: MissingReq[];
  cycles: string[][];
  edgesBySource: Map<string, DepEdge[]>;
  missingBySource: Map<string, MissingReq[]>;
}

function pushTo<K, V>(map: Map<K, V[]>, key: K, value: V): void {
  const bucket = map.get(key);
  if (bucket) bucket.push(value);
  else map.set(key, [value]);
}

/** Build the same-manifest dependency graph: edges from `display.requires`,
 *  missing references flagged, cycles found via Tarjan SCC. */
export function resolveDeps(mods: ModEntry[]): DepGraph {
  const present = new Set(mods.map((m) => m.filename));
  const edges: DepEdge[] = [];
  const missing: MissingReq[] = [];
  const edgesBySource = new Map<string, DepEdge[]>();
  const missingBySource = new Map<string, MissingReq[]>();

  for (const m of mods) {
    for (const req of m.display?.requires ?? []) {
      if (present.has(req.filename)) {
        const edge: DepEdge = {
          from: m.filename,
          to: req.filename,
          versionRange: req.version_range ?? null,
          optional: req.optional,
        };
        edges.push(edge);
        pushTo(edgesBySource, m.filename, edge);
      } else {
        const miss: MissingReq = { from: m.filename, requires: req.filename };
        missing.push(miss);
        pushTo(missingBySource, m.filename, miss);
      }
    }
  }
  return {
    edges,
    missing,
    cycles: findCycles(
      mods.map((m) => m.filename),
      edges,
    ),
    edgesBySource,
    missingBySource,
  };
}

// Tarjan SCC; keep components of size > 1, plus single nodes with a self-loop.
function findCycles(nodes: string[], edges: DepEdge[]): string[][] {
  const adj = new Map<string, string[]>();
  const selfLoop = new Set<string>();
  for (const n of nodes) adj.set(n, []);
  for (const e of edges) {
    pushTo(adj, e.from, e.to);
    if (e.from === e.to) selfLoop.add(e.from);
  }

  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const sccs: string[][] = [];
  let counter = 0;

  const connect = (v: string): void => {
    index.set(v, counter);
    low.set(v, counter);
    counter++;
    stack.push(v);
    onStack.add(v);
    for (const w of adj.get(v) ?? []) {
      if (!index.has(w)) {
        connect(w);
        low.set(v, Math.min(low.get(v) ?? 0, low.get(w) ?? 0));
      } else if (onStack.has(w)) {
        low.set(v, Math.min(low.get(v) ?? 0, index.get(w) ?? 0));
      }
    }
    if ((low.get(v) ?? 0) === (index.get(v) ?? 0)) {
      const comp: string[] = [];
      let w: string;
      do {
        w = stack.pop() as string;
        onStack.delete(w);
        comp.push(w);
      } while (w !== v);
      sccs.push(comp);
    }
  };

  for (const n of nodes) if (!index.has(n)) connect(n);
  return sccs.filter((c) => c.length > 1 || (c.length === 1 && selfLoop.has(c[0])));
}

// ── Incompatibilities (bidirectional, per OptionalContentRules.conflicts) ────

/** filename -> set of present mods it conflicts with (declared either way). */
export function conflictIndex(mods: ModEntry[]): Map<string, Set<string>> {
  const present = new Set(mods.map((m) => m.filename));
  const out = new Map<string, Set<string>>();
  const link = (a: string, b: string) => {
    const set = out.get(a) ?? new Set<string>();
    set.add(b);
    out.set(a, set);
  };
  for (const m of mods) {
    for (const other of m.display?.incompatible_with ?? []) {
      if (!present.has(other)) continue; // only flag conflicts within this pack
      link(m.filename, other);
      link(other, m.filename);
    }
  }
  return out;
}

// ── Content-tab section ordering ─────────────────────────────────────────────

const LIBRARY_CATEGORIES = new Set(['lib', 'library']);

export function isLibrary(m: ModEntry): boolean {
  return LIBRARY_CATEGORIES.has((m.display?.category ?? '').trim().toLowerCase());
}

export interface AssetBuckets {
  resourcepacks: AssetEntry[];
  shaderpacks: AssetEntry[];
  configs: AssetEntry[];
  other: AssetEntry[];
}

export function bucketAssets(assets: AssetEntry[]): AssetBuckets {
  const buckets: AssetBuckets = { resourcepacks: [], shaderpacks: [], configs: [], other: [] };
  for (const a of assets) {
    if (a.dest.startsWith('resourcepacks/')) buckets.resourcepacks.push(a);
    else if (a.dest.startsWith('shaderpacks/')) buckets.shaderpacks.push(a);
    else if (a.dest.startsWith('config/')) buckets.configs.push(a);
    else buckets.other.push(a);
  }
  return buckets;
}

// ── Display helpers ──────────────────────────────────────────────────────────

export function modName(item: { filename: string; display?: Display | null }): string {
  return item.display?.name?.trim() || item.filename;
}

export function assetName(item: { dest: string; display?: Display | null }): string {
  return item.display?.name?.trim() || item.dest.split('/').pop() || item.dest;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
