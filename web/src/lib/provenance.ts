// Who published the exact bytes of one artifact, as the chip beside it.
//
// The panel used to ask a two-way question -- Modrinth or not -- and "not" was
// drawn as self-hosted. That reads as a verdict when it was only an absence:
// every jar from CurseForge, and every jar nobody had asked about yet, landed
// in the same bucket as a genuinely unpublished one.

import type { MsgKey } from './i18n.svelte';
import type { VersionRow } from './bindings/VersionRow';

export type Provenance = {
  /** Locale key for the chip text. */
  key: MsgKey;
  /** Locale key for its title, when there is something to add. */
  hintKey?: MsgKey;
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
 *
 * Exactly one state is drawn as a warning, and it is the one this mirror exists
 * to catch: bytes under a mod its publisher does carry, which the publisher did
 * not build. That is not proof of tampering on its own -- a fork build, a local
 * build and a recompile against a changed API all land here too -- but it is
 * the only state worth stopping on, so nothing else competes for the colour.
 */
export function fileProvenance(f: VersionRow, modHasVerified: boolean): Provenance {
  if (f.modrinth_version_id) {
    return { key: 'mm.verified', cls: 'verified' };
  }
  const cf = f.curseforge;
  if (cf?.project_id) {
    const name = cf.display_name ?? cf.file_name ?? undefined;
    // Both are the publisher's own bytes, so both read as published. The
    // redistribution bar is a limit on us, not a doubt about the file, and
    // colouring it like a suspected repackage would spend the one warning the
    // list has on the thing that is not the problem.
    return cf.distributable
      ? { key: 'mm.curseforge', hintKey: 'mm.curseforgeHint', name, cls: 'verified' }
      : { key: 'mm.curseforgeNoDist', hintKey: 'mm.curseforgeNoDistHint', name, cls: 'verified' };
  }
  // Both registries have to have answered before anything is said about the
  // file. A jar the mirror does not hold cannot be fingerprinted, so CurseForge
  // was never asked about it, and Modrinth's "no" alone would put the warning on
  // half the evidence: the genuine Ender IO 5.2.61 sits exactly here.
  if (!cf) {
    return { key: 'mm.unasked', hintKey: 'mm.unaskedHint' };
  }
  if (modHasVerified) {
    return { key: 'mm.notPublisher', hintKey: 'mm.notPublisherHint', cls: 'repack' };
  }
  return { key: 'mm.unpublished', hintKey: 'mm.unpublishedHint' };
}
