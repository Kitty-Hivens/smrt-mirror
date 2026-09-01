// Compare a freshly-computed (dry-run) manifest against the currently-published
// one. Drives the "what would publishing change?" panel.
//
// Done here rather than by the mirror because a dry run has no published
// version to name: `/v1/packs/{id}/diff` answers between two builds that exist.
// It must still give the same answer the mirror would, which means matching
// mods the same way -- see `identity`.

import type { ModEntry, PackManifest } from './types';

/// What makes two entries the same mod across builds: the Modrinth project, else
/// the curator slug (ADR 0002), else the filename.
///
/// The same rule as `ModEntry::identity` in `domain/diff.rs`, and it has to be:
/// matching on the filename alone -- which is what this did -- reads a re-pin
/// that renames the jar as a removal plus an addition, while the update dialog
/// a player sees calls the same event an update. One product, one answer.
function identity(m: ModEntry): string {
  if (m.source.type === 'modrinth') return `m:${m.source.project_id}`;
  return m.slug ? `s:${m.slug}` : `f:${m.filename}`;
}

export interface ModChange {
  filename: string;
  prevSha1: string;
  nextSha1: string;
}

export interface ManifestDiff {
  added: ModEntry[];
  removed: ModEntry[];
  changed: ModChange[];
  unchanged: number;
  prevVersion: string;
  nextVersion: string;
}

export function diffManifests(prev: PackManifest, next: PackManifest): ManifestDiff {
  const prevById = new Map(prev.mods.map((m) => [identity(m), m]));
  const nextIds = new Set(next.mods.map(identity));

  const added: ModEntry[] = [];
  const changed: ModChange[] = [];
  let unchanged = 0;

  for (const m of next.mods) {
    const before = prevById.get(identity(m));
    if (!before) added.push(m);
    else if (before.sha1 !== m.sha1)
      changed.push({ filename: m.filename, prevSha1: before.sha1, nextSha1: m.sha1 });
    else unchanged++;
  }

  const removed = prev.mods.filter((m) => !nextIds.has(identity(m)));

  return {
    added,
    removed,
    changed,
    unchanged,
    prevVersion: prev.pack_version,
    nextVersion: next.pack_version,
  };
}

export function diffIsEmpty(d: ManifestDiff): boolean {
  return d.added.length === 0 && d.removed.length === 0 && d.changed.length === 0;
}
