// Wire DTOs are generated from the Rust structs by ts-rs -- see bindings/,
// regenerated with `TS_RS_EXPORT_DIR=web/src/lib cargo test` from the smrt
// crate root. This barrel re-exports them so the panel imports stay stable.
// Operational / external types that have no Rust counterpart stay hand-written
// at the bottom.

import type { DryRun } from './bindings/DryRun';

export type { DryRun };
export type { Health } from './bindings/Health';
export type { ManifestBuildInfo } from './bindings/ManifestBuildInfo';
export type { PackSummary } from './bindings/PackSummary';
export type { PackListing } from './bindings/PackListing';
export type { CommunityPack } from './bindings/CommunityPack';
export type { ManifestVersionsListing } from './bindings/ManifestVersionsListing';
export type { AuthoringPacksListing } from './bindings/AuthoringPacksListing';
export type { ServerEntry } from './bindings/ServerEntry';
export type { ServerListing } from './bindings/ServerListing';
export type { Featured } from './bindings/Featured';
export type { CacheInventory } from './bindings/CacheInventory';
export type { CacheInventoryEntry } from './bindings/CacheInventoryEntry';
export type { CacheUsageListing } from './bindings/CacheUsageListing';
export type { CacheUsageEntry } from './bindings/CacheUsageEntry';
export type { CacheUse } from './bindings/CacheUse';

// authoring config
export type { PackConfig } from './bindings/PackConfig';
export type { PackMeta } from './bindings/PackMeta';
export type { PackTier } from './bindings/PackTier';
export type { Visibility } from './bindings/Visibility';
export type { DeclaredMod } from './bindings/DeclaredMod';
export type { DeclaredAsset } from './bindings/DeclaredAsset';
export type { SourceDecl } from './bindings/SourceDecl';
export type { LoaderSpec } from './bindings/LoaderSpec';
export type { Display } from './bindings/Display';

// wire manifest (for the launcher-faithful preview)
export type { PackManifest } from './bindings/PackManifest';
export type { ModEntry } from './bindings/ModEntry';
export type { AssetEntry } from './bindings/AssetEntry';
export type { Source } from './bindings/Source';
export type { Requirement } from './bindings/Requirement';

// validate report (config vs an instance archive)
export type { ValidateReport } from './bindings/ValidateReport';

// resolve report (config vs registry dependency graph)
export type { LoaderFit } from './bindings/LoaderFit';
export type { ModHit } from './bindings/ModHit';
export type { PackEvent } from './bindings/PackEvent';
export type { PulledPreview } from './bindings/PulledPreview';
export type { ResolveReport } from './bindings/ResolveReport';
export type { MissingDep } from './bindings/MissingDep';
export type { ActiveConflict } from './bindings/ActiveConflict';
export type { CapabilityOverlap } from './bindings/CapabilityOverlap';
export type { VersionIssue } from './bindings/VersionIssue';

// accounts (users + roles)
export type { UserRow } from './bindings/UserRow';
export type { UploadRow } from './bindings/UploadRow';
export type { AuditRow } from './bindings/AuditRow';

// registry browser (mods + builds, faceted)
export type { JarDiff } from './bindings/JarDiff';
export type { GraphData } from './bindings/GraphData';
export type { GraphNode } from './bindings/GraphNode';
export type { GraphEdge } from './bindings/GraphEdge';
export type { ModrinthProjectName } from './bindings/ModrinthProjectName';
export type { GraphSlice } from './bindings/GraphSlice';
export type { ModSummary } from './bindings/ModSummary';
export type { ModDetail } from './bindings/ModDetail';
export type { ModEdge } from './bindings/ModEdge';
export type { VersionRow } from './bindings/VersionRow';
export type { ReleaseRow } from './bindings/ReleaseRow';
export type { UnassignedJar } from './bindings/UnassignedJar';
export type { BuildSummary } from './bindings/BuildSummary';
export type { Thread } from './bindings/Thread';
export type { ThreadComment } from './bindings/ThreadComment';
export type { ThreadView } from './bindings/ThreadView';
export type { PackGrant } from './bindings/PackGrant';
export type { PackBlock } from './bindings/PackBlock';
export type { Notification } from './bindings/Notification';
export type { PackLevel } from './bindings/PackLevel';
export type { Commit } from './bindings/Commit';
export type { CommitLogEntry } from './bindings/CommitLogEntry';
export type { CommitStatus } from './bindings/CommitStatus';
export type { CommitDiff } from './bindings/CommitDiff';
export type { ConfigChange } from './bindings/ConfigChange';
export type { ChangeGroup } from './bindings/ChangeGroup';
export type { ChangeOp } from './bindings/ChangeOp';
export type { ChangeField } from './bindings/ChangeField';
export type { BuildModRow } from './bindings/BuildModRow';
export type { ModUse } from './bindings/ModUse';

// ── hand-written: operational + external (no Rust DTO) ──

export type JobStatus = 'running' | 'done' | 'failed';

export interface ModrinthHit {
  project_id: string;
  slug: string;
  title: string;
  description: string;
  icon_url?: string | null;
  author?: string;
}

export interface ModrinthVersion {
  id: string;
  project_id: string;
  version_number: string;
  // release channel: 'release' | 'beta' | 'alpha'
  version_type?: string;
  game_versions: string[];
  loaders: string[];
  // Upstream publishes a version whose jar never landed as an empty array; such
  // a pin resolves to nothing at build time, so the picker will not offer it.
  files: { filename: string; size: number }[];
}

// GET /v1/jobs/:id -- `result` is present only for a finished dry-run.
export interface JobResult {
  job_id: string;
  kind: string;
  pack_id: string;
  status: JobStatus;
  log: string[];
  // What the pre-publish check would stop this build on. Absent when it found
  // nothing; present on a preview too, where it is a warning rather than a
  // refusal.
  blocked?: string[];
  result?: DryRun | null;
}

// GET /v1/meta/minecraft -- the version list the editor offers, from the
// mirror's copy. `stale` means upstream was unreachable and this is the last
// known list, which is a better answer than an empty picker.
export interface GameVersion {
  version: string;
  version_type: string;
  date: string;
  major: boolean;
}

export interface MinecraftVersions {
  versions: GameVersion[];
  fetched_at: string;
  fetched_unix: number;
  stale: boolean;
}

// GET/POST /v1/authoring/packs/:id/spoof -- the FML handshake claim a pack
// ships against what its server expects now (#110). `unasked` present means
// the server was not asked, or said nothing usable, and explains which.
export interface Spoof {
  mods: { id: string; version: string }[];
}

export interface SpoofReport {
  shipped?: Spoof | null;
  current?: Spoof | null;
  server_id?: string | null;
  asked?: string | null;
  unasked?: string | null;
  drift: string[];
}

// GET /v1/meta/loaders/:loader -- the builds a pack can be pinned to (#126).
// `minecraft` is absent for loaders whose builds do not tie to one (Fabric,
// Quilt). `stale` means upstream was unreachable and this is the last known.
export interface LoaderBuild {
  version: string;
  minecraft?: string | null;
  recommended: boolean;
  latest: boolean;
}

export interface LoaderVersions {
  loader: string;
  builds: LoaderBuild[];
  fetched_at: string;
  fetched_unix: number;
  stale: boolean;
}
