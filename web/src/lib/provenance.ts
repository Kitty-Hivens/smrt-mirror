// Who published the exact bytes of one artifact, as the chip beside it.
//
// The panel used to ask a two-way question -- Modrinth or not -- and "not" was
// drawn as self-hosted. That reads as a verdict when it was only an absence:
// every jar from CurseForge, and every jar nobody had asked about yet, landed
// in the same bucket as a genuinely unpublished one.

import type { VersionRow } from './bindings/VersionRow';

export type Provenance = {
  /** Locale key for the chip text. */
  key: string;
  /** Locale key for its title, when there is something to add. */
  hintKey?: string;
  /** Interpolated into the hint: the name the publisher gave the file. */
  name?: string;
  /** Chip modifier class, matching the existing `verified` / `repack`. */
  cls?: string;
};

/**
 * `modHasVerified` is whether any artifact of the owning mod is Modrinth's own,
 * which is what makes a sibling that is not look like a repackage.
 *
 * Order is deliberate. A file either publisher serves is theirs and that is the
 * end of it. Only once neither does can it be a repackage, and only once
 * CurseForge has actually answered can it be called unpublished: no row means
 * the question was never put, which is a different thing to say.
 */
export function fileProvenance(f: VersionRow, modHasVerified: boolean): Provenance {
  if (f.modrinth_version_id) {
    return { key: 'mm.verified', cls: 'verified' };
  }
  const cf = f.curseforge;
  if (cf?.project_id) {
    const name = cf.display_name ?? cf.file_name ?? undefined;
    return cf.distributable
      ? { key: 'mm.curseforge', hintKey: 'mm.curseforgeHint', name, cls: 'verified' }
      : { key: 'mm.curseforgeNoDist', hintKey: 'mm.curseforgeNoDistHint', name, cls: 'repack' };
  }
  if (modHasVerified) {
    return { key: 'mm.repack', hintKey: 'mm.repackHint', cls: 'repack' };
  }
  if (cf) {
    return { key: 'mm.selfhost', hintKey: 'mm.selfhostHint' };
  }
  return { key: 'mm.unasked', hintKey: 'mm.unaskedHint' };
}
