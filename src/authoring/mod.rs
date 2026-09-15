//! Authoring (service layer): turn an admin-authored `PackConfig` into the
//! wire manifest, bootstrap a starter config from an instance archive, run the
//! build enrichment passes, and resolve Modrinth sources. The compute core
//! shared by the `smrt-pack` CLI and the panel's build endpoints. `archive`
//! and `sources` are internal helpers; the passes are the public surface.

mod archive;
mod remotezip;
mod sources;

pub mod bootstrap;
pub mod build;
pub mod bytecode;
pub mod classfile;
pub mod commits;
pub mod configdiff;
pub mod curator;
pub mod curseforge;
pub mod depfill;
pub mod gate;
pub mod github;
pub mod harvest;
pub mod harvest_sched;
pub mod jardiff;
pub mod loaderreq;
pub mod loaders;
pub mod mcping;
pub mod mixinscan;
pub mod modmeta;
pub mod modrinth;
pub mod packdoc;
pub mod packstream;
pub mod reconstruct;
pub mod resolve;
pub mod search;
pub mod spoof;
pub mod validate;
pub mod versions;

pub use bootstrap::{BootstrapArgs, bootstrap};
pub use build::{Built, build_manifest, make_pack_summary};
pub use commits::{Commit, CommitSnapshot, CommitStatus, make_commit};
pub use configdiff::{
    ChangeField, ChangeGroup, ChangeOp, ConfigChange, diff_configs, initial, uncommitted,
    whole_config,
};
pub use curator::{
    McModInfo, enrich_from_mcmod_info, infer_requires_from_mcmod_info, jar_icon, read_mcmod_info,
};
pub use harvest_sched::{HarvestScheduler, HarvestStatus};
pub use jardiff::{JarDiff, diff_jars};
pub use loaderreq::{LoaderWindowReport, loader_windows};
pub use loaders::{LoaderVersions, loader_versions};
pub use mcping::{ServerStatus, status as server_status};
pub use modrinth::*;
pub use packdoc::{PackDoc, PackDocs};
pub use packstream::{PackEvent, PackStream, Presence};
pub use reconstruct::reconstruct_config;
pub use resolve::{ResolveReport, pack_graph, resolve_pack};
pub use search::{ModHit, PackContext, search_mods};
pub use spoof::{SPOOF_DEST, Spoof, SpoofMod, spoof_from_status};
pub use validate::{ValidateReport, validate};
pub use versions::{MinecraftVersions, minecraft_versions};
