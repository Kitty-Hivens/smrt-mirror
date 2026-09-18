// Compare a freshly-computed (dry-run) manifest against the currently-published
// one. Drives the "what would publishing change?" panel.
//
// Done here rather than by the mirror because a dry run has no published
// version to name: `/v1/packs/{id}/diff` answers between two builds that exist.
// It must still give the same answer the mirror would, which means matching
// mods the same way -- see `identity`.

import type { ModEntry, PackManifest } from './types';

/// What makes two entries the same mod across builds: the curator slug (ADR
/// 0002), else the publisher project the entry names, else the filename.
///
/// The same rule as `ModEntry::identity` in `domain/diff.rs`, and it has to be:
/// this one answers for a dry run, which has no published build for the mirror
/// to diff against, and the two must not call one event by two names. One
/// product, one answer.
///
/// The slug leads because it is the only part that survives a move between
/// publishers, which is exactly the move this panel is used to make.
function identity(m: ModEntry): string {
  const slug = m.slug?.trim();
  if (slug) return `s:${slug}`;
  if (m.source.type === 'modrinth') return `m:${m.source.project_id}`;
  if (m.source.type === 'curseforge') return `cf:${m.source.project_id}`;
  // the repository alone: the tag is the version by definition and the asset
  // name carries it just as often, so either would re-key the entry at every
  // release
  if (m.source.type === 'github') return `gh:${m.source.repo}`;
  return `f:${m.filename}`;
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
