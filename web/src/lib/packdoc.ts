// One pack's config, as the mirror merges it (#115).
//
// The mirror holds a document per pack; this is the editor's copy of it. The
// panel keeps editing a plain config object with plain `bind:value` controls --
// rewriting every control to bind to a CRDT node would be a rewrite of the
// editor for no gain -- so the two are kept in step by two functions: read the
// document into a config, and write a config's changes into the document.
//
// Writing is a diff, not a replace, and that is the whole of the difficulty.
// Assigning a whole string to a text node is a delete of everything followed by
// an insert of everything, which is exactly what destroys a concurrent edit: the
// other person's sentence is inside the range being deleted. So prose is patched
// at the smallest range that changed, which is what makes two people typing in
// one paragraph merge rather than overwrite.

import * as Y from 'yjs';
import type { PackConfig } from './types';

/// The document's one root, and the prose paths. Both mirror `packdoc.rs` --
/// they describe one document, and a disagreement about its shape between the
/// two ends is not a thing that fails loudly.
const ROOT = 'config';
const PROSE = new Set(['tagline', 'pack_meta.description_md']);
/// The same two fields written in another language (#195). The tag is part of
/// the path, so these are prefixes rather than a list nobody can write ahead of
/// time -- and a translation is composed the way the original is, so it has to
/// merge the way the original does.
const PROSE_BY_LANGUAGE = ['pack_meta.tagline_i18n.', 'pack_meta.description_md_i18n.'];

/// Fields the server owns. They never enter the document, so the editor neither
/// sends nor expects them; it keeps whatever the config it loaded said.
const SERVER_OWNED = ['owner', 'tier', 'visibility', 'fork_of'] as const;

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

export function isProse(path: string): boolean {
  return PROSE.has(path) || PROSE_BY_LANGUAGE.some((prefix) => path.startsWith(prefix));
}

function childPath(parent: string, key: string): string {
  return parent ? `${parent}.${key}` : key;
}

// ── reading ─────────────────────────────────────────────────────────────────

function fromShared(value: unknown): Json {
  if (value instanceof Y.Text) return value.toString();
  if (value instanceof Y.Array) return value.toArray().map(fromShared);
  if (value instanceof Y.Map) {
    const out: { [key: string]: Json } = {};
    for (const [key, child] of value.entries()) out[key] = fromShared(child);
    return out;
  }
  return (value ?? null) as Json;
}

/// The document as a config, with the server's fields taken from `base` -- they
/// were never in the document to be read.
export function readConfig(doc: Y.Doc, base: PackConfig): PackConfig {
  const root = doc.getMap(ROOT);
  const out = fromShared(root) as unknown as Record<string, unknown>;
  for (const key of SERVER_OWNED) out[key] = (base as unknown as Record<string, unknown>)[key];
  return out as unknown as PackConfig;
}

// ── writing ─────────────────────────────────────────────────────────────────

/// The smallest edit that turns `from` into `to`: skip the common prefix, skip
/// the common suffix, replace what is left.
///
/// Not a real diff, and deliberately so. Typing appends, backspacing removes at
/// a point, and pasting replaces a selection -- all three are one contiguous
/// range, which this finds exactly. A two-place edit between frames collapses
/// into the span covering both, which is still smaller than the document and
/// still leaves someone else's paragraph alone.
export function textPatch(
  from: string,
  to: string,
): { index: number; remove: number; insert: string } | null {
  if (from === to) return null;
  let start = 0;
  const max = Math.min(from.length, to.length);
  while (start < max && from[start] === to[start]) start++;
  let tail = 0;
  while (tail < max - start && from[from.length - 1 - tail] === to[to.length - 1 - tail]) tail++;
  return {
    index: start,
    remove: from.length - start - tail,
    insert: to.slice(start, to.length - tail),
  };
}

function toShared(path: string, value: Json): unknown {
  if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
    const map = new Y.Map();
    for (const [key, child] of Object.entries(value)) {
      map.set(key, toShared(childPath(path, key), child) as never);
    }
    return map;
  }
  if (Array.isArray(value)) {
    const array = new Y.Array();
    array.push(value.map((item) => toShared(path, item)) as never[]);
    return array;
  }
  if (typeof value === 'string' && isProse(path)) return new Y.Text(value);
  return value;
}

function writeMap(map: Y.Map<unknown>, path: string, next: { [key: string]: Json }) {
  for (const key of Object.keys(next)) {
    writeInto(map, key, childPath(path, key), next[key]);
  }
  for (const key of [...map.keys()]) {
    if (!(key in next)) map.delete(key);
  }
}

function writeArray(array: Y.Array<unknown>, path: string, next: Json[]) {
  // Rows are replaced positionally, and the array is only resized at its end.
  // A row is a row: the mirror merges two people adding rows, and this is the
  // local half of the same idea -- an edit to row three must not rewrite rows
  // one and two, which is what assigning the whole array would do.
  const shared = array.length;
  for (let i = 0; i < Math.min(shared, next.length); i++) {
    const current = array.get(i);
    if (current instanceof Y.Map && next[i] !== null && typeof next[i] === 'object' && !Array.isArray(next[i])) {
      writeMap(current as Y.Map<unknown>, path, next[i] as { [key: string]: Json });
    } else if (JSON.stringify(fromShared(current)) !== JSON.stringify(next[i])) {
      array.delete(i, 1);
      array.insert(i, [toShared(path, next[i])] as never[]);
    }
  }
  if (next.length > shared) {
    array.insert(shared, next.slice(shared).map((v) => toShared(path, v)) as never[]);
  } else if (next.length < shared) {
    array.delete(next.length, shared - next.length);
  }
}

function writeInto(map: Y.Map<unknown>, key: string, path: string, value: Json) {
  const current = map.get(key);

  if (current instanceof Y.Text && typeof value === 'string') {
    const patch = textPatch(current.toString(), value);
    if (!patch) return;
    if (patch.remove) current.delete(patch.index, patch.remove);
    if (patch.insert) current.insert(patch.index, patch.insert);
    return;
  }
  if (current instanceof Y.Map && value !== null && typeof value === 'object' && !Array.isArray(value)) {
    writeMap(current as Y.Map<unknown>, path, value as { [key: string]: Json });
    return;
  }
  if (current instanceof Y.Array && Array.isArray(value)) {
    writeArray(current as Y.Array<unknown>, path, value);
    return;
  }
  if (JSON.stringify(fromShared(current)) === JSON.stringify(value)) return;
  map.set(key, toShared(path, value) as never);
}

/// Put a config's changes into the document, touching only what differs. The
/// server's fields are skipped: the document does not carry them.
export function writeConfig(doc: Y.Doc, next: PackConfig, origin: unknown) {
  const root = doc.getMap(ROOT);
  const plain = JSON.parse(JSON.stringify(next)) as { [key: string]: Json };
  for (const key of SERVER_OWNED) delete plain[key];
  doc.transact(() => writeMap(root, '', plain), origin);
}
