//! Pack identity as the launcher sees it (`PackSummary`, listings) plus the
//! admin-authored `PackConfig` the build pipeline consumes. The config is a
//! distinct type from the wire manifest: authoring does not hand-write
//! `sha1` / `size_bytes` for Modrinth sources -- those are resolved at build.

use super::manifest::{AuthSpec, Display, LoaderSpec};
use super::version::VersionChannel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ts_rs::TS;
use utoipa::ToSchema;

// ── Ownership ──────────────────────────────────────────────────────────────

/// The deployment operator's GitHub uid, from `SMRT_OPERATOR_UID`, read once.
/// Packs authored before ownership existed backfill their `owner` to it, and
/// operator-authored packs default to it. 0 (no operator identity) when the
/// variable is unset -- a fresh self-hosted instance works before one is
/// configured. Lives here rather than `Config` because serde `default` fns
/// take no arguments; the one env read is cached for the process lifetime.
fn operator_uid() -> i64 {
    static UID: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        std::env::var("SMRT_OPERATOR_UID")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    })
}

/// Curation tier. `official` = the mirror's own packs (the launcher's catalog,
/// no personal byline); `community` = a member's pack. The launcher reads
/// official only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum PackTier {
    Official,
    Community,
}

/// Publication state. Only `published` packs reach the public listing; `draft`
/// is work-in-progress, `unlisted` is reachable by direct id but off the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Draft,
    Unlisted,
    Published,
}

/// Backfill defaults, chosen so an existing config/summary with none of these
/// fields reads as an owned, official, published pack (which is what every pack
/// predating ownership is). `pub(crate)` so authoring code that mints a fresh
/// operator pack reuses the same defaults instead of re-hardcoding them.
pub(crate) fn default_owner() -> i64 {
    operator_uid()
}
pub(crate) fn default_tier() -> PackTier {
    PackTier::Official
}
pub(crate) fn default_visibility() -> Visibility {
    Visibility::Published
}

// ── Pack summary / listing ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct PackSummary {
    pub pack_id: String,
    pub display_name: String,
    pub tagline: String,
    pub minecraft_version: String,
    pub latest_pack_version: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub featured: bool,
    /// Square pack icon. Renders in BrowsePackCard avatar slot +
    /// BrowsePackDetail hero on the launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_url: Option<String>,
    /// Wide hero image. Renders behind BrowsePackDetail hero text;
    /// falls back to the launcher's mirror gradient when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub banner_url: Option<String>,
    /// Optional marketing screenshots. Rendered in a horizontal
    /// scroller on BrowsePackDetail when non-empty.
    #[serde(default)]
    pub gallery_urls: Vec<String>,
    /// Long-form CommonMark description for the BrowsePackDetail
    /// About section. HTML is not parsed by the launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description_md: Option<String>,
    /// The same tagline keyed by language tag (`{"en": "...", "ru": "..."}`),
    /// for a reader who does not read the language the untagged one is written
    /// in. Same rule as `PackManifest::changelog_i18n`: prefer the user's
    /// language, fall back to the untagged field, and read a missing key as
    /// "not written" rather than "written as nothing".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tagline_i18n: Option<std::collections::BTreeMap<String, String>>,
    /// The same description per language tag; see `tagline_i18n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description_md_i18n: Option<std::collections::BTreeMap<String, String>>,
    /// GitHub uid of the pack owner. Official packs are owned by the operator;
    /// community packs by their member. Server-controlled -- set at authoring.
    #[serde(default = "default_owner")]
    #[ts(type = "number")]
    pub owner: i64,
    #[serde(default = "default_tier")]
    pub tier: PackTier,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
    /// Source pack id when this pack is a fork; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fork_of: Option<String>,
    /// When the latest build was produced (the latest manifest's
    /// `generated_at`, RFC 3339). Derived at read time from the manifest --
    /// never persisted in `summary.json`; absent when the pack has no build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub latest_built_at: Option<String>,
    /// Channel of `latest_pack_version`; same derivation and absence rules
    /// as `latest_built_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub latest_channel: Option<VersionChannel>,
}

#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct PackListing {
    pub schema_version: u32,
    pub generated_at: String,
    pub packs: Vec<PackSummary>,
}

/// A published community pack for the public Community listing: the pack summary
/// plus the owner's GitHub login (resolved from the uid) for the `by <user>`
/// byline. Community packs are browseable on the site but never in the launcher's
/// official `/v1/packs` catalog.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct CommunityPack {
    pub summary: PackSummary,
    pub owner_login: String,
}

/// One retained build of a pack, as the version picker sees it: the version
/// label plus the metadata a launcher needs to render and order a list of
/// builds without fetching each manifest. Field names follow the Modrinth
/// version object (`version_number` / `version_type` / `date_published`).
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ManifestBuildInfo {
    pub version_number: String,
    /// The manifest's stored channel; for manifests predating the field,
    /// recovered from the legacy string rule (`SNAPSHOT-` prefix = beta).
    pub version_type: VersionChannel,
    /// The manifest's `generated_at` (RFC 3339): when the build produced it.
    pub date_published: String,
    /// The manifest's content fingerprint where present -- the reliable
    /// "did the content change?" signal between two builds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fingerprint: Option<String>,
    /// The commit this build was made from (#122), where it names one. Absent
    /// on CLI builds, which build the working config, and on every build from
    /// before a pack had history. Carried in the listing so "which builds came
    /// out of this checkpoint" is answerable without opening every manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub built_from: Option<String>,
    /// Curator-authored release notes, where the build carries them.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub changelog: Option<String>,
    /// The same notes per language tag; see `PackManifest::changelog_i18n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub changelog_i18n: Option<std::collections::BTreeMap<String, String>>,
    #[ts(type = "number")]
    pub mods_count: u64,
    #[ts(type = "number")]
    pub assets_count: u64,
    /// What this build runs on. Present per build rather than per pack because
    /// a pack outlives its target: telling a player that an update moves them
    /// from 1.12.2 to 1.20.1, or off Forge, is exactly what a version list is
    /// read for, and answering it from the listing costs one request instead of
    /// one manifest per build.
    pub minecraft_version: String,
    pub loader: LoaderSpec,
    /// Every file this build lists, added up: what a fresh install downloads.
    /// Optional entries are counted too -- what an individual player ends up
    /// fetching depends on what they enable, and that answer only exists in the
    /// manifest.
    #[ts(type = "number")]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ManifestVersionsListing {
    pub schema_version: u32,
    pub pack_id: String,
    /// The version `manifests/latest` currently points at; absent when the
    /// pack has no published build.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub latest: Option<String>,
    /// Per-build metadata, newest first (ordered by `date_published`).
    pub builds: Vec<ManifestBuildInfo>,
}

/// Pack ids that carry editable authoring inputs (a config.json under
/// `packs/<id>/authoring/`), including packs not yet built. Admin-only:
/// authoring inputs are not part of the public read API.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct AuthoringPacksListing {
    pub schema_version: u32,
    pub packs: Vec<String>,
}

// ── Pack config (admin-authored authoring input) ───────────────────────────

/// Admin-authored declaration of a pack. The build step turns this into a wire
/// `PackManifest` by resolving each source against Modrinth or the local
/// storage tree. Distinct from `PackManifest` because authoring does not
/// require admin to hand-write `sha1` and `size_bytes` for Modrinth sources --
/// those are looked up at build time.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PackConfig {
    pub pack_id: String,
    pub display_name: String,
    pub tagline: String,
    pub minecraft_version: String,
    pub loader: LoaderSpec,
    pub java_major: u32,
    /// Human semver-ish line for the build version string
    /// (`SNAPSHOT-<version>-<date>`). Pre-1.0 packs sit at `0.0.x`; the operator
    /// bumps it rarely. Absent -> `0.0.0`. The date + same-day counter advance
    /// automatically, so this is the only version part anyone hand-edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub featured: bool,
    pub mods: Vec<DeclaredMod>,
    #[serde(default)]
    pub assets: Vec<DeclaredAsset>,
    /// Auth precondition copied verbatim onto every built manifest
    /// (`smartycraft`/`microsoft`/`both` + the SC server id where relevant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth: Option<AuthSpec>,
    #[serde(default)]
    pub pack_meta: PackMeta,
    /// GitHub uid of the pack owner. Official packs are owned by the operator;
    /// community packs by their member. Server-controlled -- never taken from a
    /// client config edit (see `put_pack_config`).
    #[serde(default = "default_owner")]
    #[ts(type = "number")]
    pub owner: i64,
    #[serde(default = "default_tier")]
    pub tier: PackTier,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
    /// Source pack id when this pack is a fork; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fork_of: Option<String>,
}

impl PackConfig {
    /// Revision of everything a client authors in this config, for the
    /// conditional write on the config PUT.
    ///
    /// Deliberately not a hash of the stored file. `owner` / `tier` /
    /// `visibility` / `fork_of` are server-controlled and depfill-appended
    /// `pulled` mods are server-managed, so a client can never be the cause of
    /// a change in them -- publishing a pack, or a fill pulling in a library,
    /// must not make the next edit look like it lost a race. What remains is
    /// exactly what two editors can collide on.
    pub fn edit_rev(&self) -> Result<String, serde_json::Error> {
        let mut projected = self.clone();
        projected.owner = 0;
        projected.tier = PackTier::Official;
        projected.visibility = Visibility::Published;
        projected.fork_of = None;
        projected.mods.retain(|m| !m.pulled);
        Ok(crate::storage::sha1_hex(&serde_json::to_vec(&projected)?))
    }

    /// This config's server-controlled fields, wearing another config's authored
    /// content.
    ///
    /// What a proposal offers is the authored half -- the mods, the assets, the
    /// card, the target and the pins. What it must never carry across is who
    /// owns the pack, what tier it is, whether it is published, or which pack it
    /// was forked from: those belong to the pack being merged into, and a
    /// proposal that moved them would let a fork rename its way into ownership.
    /// The identity stays too, for the same reason.
    pub fn with_authored_from(&self, other: &PackConfig) -> PackConfig {
        PackConfig {
            pack_id: self.pack_id.clone(),
            owner: self.owner,
            tier: self.tier,
            visibility: self.visibility,
            fork_of: self.fork_of.clone(),
            ..other.clone()
        }
    }

    /// The first duplicate declaration in `mods`, described for an error
    /// message, or `None` when every row is distinct.
    ///
    /// Two things must be unique. The artifact identity -- a Modrinth project, a
    /// cache sha1, a static path -- because a pack ships one build of a mod, and
    /// two builds of the same mod in one instance is a crash, not a choice. And
    /// the filename, because the launcher writes `mods/<filename>` and a second
    /// row of the same name silently overwrites the first.
    ///
    /// Identity deliberately ignores the pinned version: the same project at two
    /// versions is still the same mod, and changing versions is a re-pin of the
    /// row rather than a second row.
    ///
    /// Assets are held to the same rule on their `dest`: two rows writing one
    /// path install one of the two, chosen by whichever the launcher happens to
    /// fetch last.
    pub fn duplicate_declaration(&self) -> Option<String> {
        let mut identities: HashSet<String> = HashSet::new();
        let mut filenames: HashSet<&str> = HashSet::new();
        for m in &self.mods {
            let identity = match &m.source {
                SourceDecl::Modrinth { project_id, .. } => {
                    format!("modrinth project {project_id}")
                }
                SourceDecl::CurseForge {
                    project_id,
                    file_id,
                } => format!("curseforge file {project_id}/{file_id}"),
                SourceDecl::Github { repo, tag, asset } => {
                    format!("github asset {repo}@{tag}/{asset}")
                }
                SourceDecl::SmrtCache { sha1 } => format!("cache jar {sha1}"),
                SourceDecl::SmrtStatic { rel_path } => format!("static file {rel_path}"),
            };
            if !identities.insert(identity.clone()) {
                return Some(format!(
                    "{identity} is declared twice (second row: {:?})",
                    m.filename
                ));
            }
            if !filenames.insert(m.filename.as_str()) {
                return Some(format!("filename {:?} is declared twice", m.filename));
            }
        }
        let mut dests: HashSet<&str> = HashSet::new();
        for a in &self.assets {
            if !dests.insert(a.dest.as_str()) {
                return Some(format!("asset dest {:?} is declared twice", a.dest));
            }
        }
        None
    }
}

/// Pack-card metadata (icon / banner / gallery / long description) merged into
/// the emitted `summary.json` at build time. Every field optional; absent fields
/// stay out of summary.json (per the `skip_serializing_if` on PackSummary).
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PackMeta {
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub banner_url: Option<String>,
    #[serde(default)]
    pub gallery_urls: Vec<String>,
    #[serde(default)]
    pub description_md: Option<String>,
    /// The card's text in the other languages this pack is read in, keyed by
    /// language tag. The two fields above stay what they are and remain what a
    /// client reads when it matches nothing here; the build settles these onto
    /// the summary (see `make_pack_summary`).
    #[serde(default)]
    pub tagline_i18n: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub description_md_i18n: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DeclaredMod {
    pub filename: String,
    /// Install-time default: whether the mod ships enabled. Every mod is toggleable
    /// -- there is no hand-set "required" flag. A mod another present mod hard-depends
    /// on is marked required on the emitted ModEntry automatically at build time.
    #[serde(default = "default_true")]
    pub default_enabled: bool,
    pub source: SourceDecl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<Display>,
    /// Curator-assigned stable identity, carried into the emitted ModEntry so the
    /// launcher can key an optional mod's toggle by it across version bumps (ADR
    /// 0002). Optional; a Modrinth mod already has a stable key in its project id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slug: Option<String>,
    /// True when depfill appended this entry as a resolved hard dependency
    /// (server-managed), false for curator-declared mods. A pulled entry is
    /// sticky: a save whose body lacks it merges it back in (a client that
    /// never saw it must not delete it), and it is dropped only when no
    /// curator-declared mod still reaches it through hard requires edges.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pulled: bool,
}

/// `skip_serializing_if` helper: omit `pulled` when false so existing configs
/// and the wire stay byte-identical for curator-declared mods.
fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DeclaredAsset {
    pub dest: String,
    #[serde(default = "default_true")]
    pub required: bool,
    pub source: SourceDecl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<Display>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceDecl {
    Modrinth {
        project_id: String,
        version_id: String,
    },
    /// A published CurseForge file, named by the two ids a pin can carry.
    ///
    /// No url here on purpose: resolving a file id to a download link needs the
    /// mirror's API key, so the build does it and the manifest carries the
    /// result. A launcher never talks to CurseForge, the same way it never needs
    /// a key of its own.
    #[serde(rename = "curseforge")]
    CurseForge {
        #[ts(type = "number")]
        project_id: i64,
        #[ts(type = "number")]
        file_id: i64,
    },
    /// An asset of a GitHub release, named by the three things that identify
    /// one. A release asset has a stable public URL, so unlike a CurseForge
    /// file id there is nothing to resolve and no key to hold: the build can
    /// write the link straight into the manifest.
    ///
    /// The asset is named because a release carries several. HookLib Ultimate
    /// ships universal, dev and dev-sources under one tag and only the first
    /// runs, so a pin that named only the repository and the tag would be a
    /// coin toss.
    #[serde(rename = "github")]
    Github {
        /// `owner/name`.
        repo: String,
        tag: String,
        asset: String,
    },
    SmrtCache {
        sha1: String,
    },
    SmrtStatic {
        rel_path: String,
    },
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_summary_round_trips_rich_metadata() {
        // r##"..."## (two hashes) -- the description_md contains "# ",
        // and r#"..."# would terminate at the first `"#` it hit. Two
        // hashes leave room for a single hash inside.
        let json = r##"{
            "pack_id": "Industrial",
            "display_name": "Industrial",
            "tagline": "Heavy industry and automation.",
            "minecraft_version": "1.12.2",
            "latest_pack_version": "2026.05.23.1",
            "tags": ["tech", "industrial"],
            "featured": true,
            "icon_url": "https://smrt.hivens.dev/v1/packs/Industrial/static/_nexira/icon.png",
            "banner_url": "https://smrt.hivens.dev/v1/packs/Industrial/static/_nexira/banner.png",
            "gallery_urls": [
                "https://smrt.hivens.dev/v1/packs/Industrial/static/_nexira/g1.png"
            ],
            "description_md": "# Industrial\n\nLong-form copy.",
            "tagline_i18n": {"ru": "Тяжёлая промышленность."},
            "description_md_i18n": {"ru": "# Индустриальный"}
        }"##;
        let s: PackSummary = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.icon_url.as_deref(),
            Some("https://smrt.hivens.dev/v1/packs/Industrial/static/_nexira/icon.png")
        );
        assert_eq!(
            s.banner_url.as_deref(),
            Some("https://smrt.hivens.dev/v1/packs/Industrial/static/_nexira/banner.png")
        );
        assert_eq!(s.gallery_urls.len(), 1);
        assert!(
            s.description_md
                .as_deref()
                .unwrap()
                .starts_with("# Industrial")
        );
        // the card in the other language it is read in, beside the untagged copy
        // rather than instead of it
        assert_eq!(
            s.tagline_i18n.as_ref().and_then(|m| m.get("ru")),
            Some(&"Тяжёлая промышленность.".to_string())
        );
        assert_eq!(
            s.description_md_i18n.as_ref().and_then(|m| m.get("ru")),
            Some(&"# Индустриальный".to_string())
        );
    }

    #[test]
    fn pack_summary_without_rich_metadata_parses() {
        // Existing summary.json files written before the rich-metadata
        // extension must still parse.
        let json = r#"{
            "pack_id": "Bare",
            "display_name": "Bare",
            "tagline": "",
            "minecraft_version": "1.12.2",
            "latest_pack_version": "2026.06.01",
            "tags": []
        }"#;
        let s: PackSummary = serde_json::from_str(json).unwrap();
        assert!(s.icon_url.is_none());
        assert!(s.banner_url.is_none());
        assert!(s.gallery_urls.is_empty());
        assert!(s.description_md.is_none());
        assert!(s.tagline_i18n.is_none() && s.description_md_i18n.is_none());
        // the ownership fields backfill via serde defaults, so a summary predating
        // them reads as an owned, official, published pack -- no migration needed
        assert_eq!(s.owner, default_owner());
        assert_eq!(s.tier, PackTier::Official);
        assert_eq!(s.visibility, Visibility::Published);
        assert!(s.fork_of.is_none());
    }

    fn config_with(mods: Vec<DeclaredMod>) -> PackConfig {
        PackConfig {
            pack_id: "t".into(),
            display_name: "t".into(),
            tagline: String::new(),
            minecraft_version: "1.21.1".into(),
            loader: LoaderSpec {
                name: "neoforge".into(),
                version: String::new(),
            },
            java_major: 21,
            version: None,
            tags: vec![],
            featured: false,
            mods,
            assets: vec![],
            auth: None,
            pack_meta: PackMeta::default(),
            owner: default_owner(),
            tier: default_tier(),
            visibility: default_visibility(),
            fork_of: None,
        }
    }

    fn mr(filename: &str, project_id: &str, version_id: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::Modrinth {
                project_id: project_id.into(),
                version_id: version_id.into(),
            },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    // One row per mod, and one row per installed filename. The version is not part
    // of the identity: the same project pinned twice is the same mod twice, which
    // is exactly the case a version-keyed check used to wave through.
    #[test]
    fn duplicate_declaration_catches_a_project_pinned_twice_and_a_reused_filename() {
        let clean = config_with(vec![
            mr("jei.jar", "PROJ_JEI", "v1"),
            mr("c.jar", "PROJ_C", "v1"),
        ]);
        assert!(clean.duplicate_declaration().is_none());

        let two_versions = config_with(vec![
            mr("jei.jar", "PROJ_JEI", "v1"),
            mr("jei-old.jar", "PROJ_JEI", "v0"),
        ]);
        let msg = two_versions
            .duplicate_declaration()
            .expect("same project twice");
        assert!(msg.contains("PROJ_JEI"), "got {msg}");

        let same_name = config_with(vec![
            mr("jei.jar", "PROJ_JEI", "v1"),
            mr("jei.jar", "PROJ_C", "v1"),
        ]);
        let msg = same_name
            .duplicate_declaration()
            .expect("same filename twice");
        assert!(msg.contains("jei.jar"), "got {msg}");

        let mut same_dest = config_with(vec![]);
        let asset = |dest: &str| DeclaredAsset {
            dest: dest.into(),
            required: true,
            source: SourceDecl::SmrtStatic {
                rel_path: format!("_nexira/{dest}"),
            },
            display: None,
        };
        same_dest.assets = vec![asset("resourcepacks/x.zip"), asset("resourcepacks/x.zip")];
        let msg = same_dest.duplicate_declaration().expect("same dest twice");
        assert!(msg.contains("resourcepacks/x.zip"), "got {msg}");
    }

    // The conditional-write rev must move on exactly what a client can author,
    // so a save is rejected when someone else edited the same fields, and only
    // then.
    #[test]
    fn a_merge_takes_content_and_leaves_ownership() {
        // what a proposal offers is the authored half; who owns the pack, what
        // tier it is and whether it is published belong to the pack merged into
        let mut target = config_with(vec![]);
        target.pack_id = "Create".into();
        target.owner = 1;
        target.tier = PackTier::Official;
        target.visibility = Visibility::Published;

        let mut offered = config_with(vec![]);
        offered.pack_id = "u/42/Create".into();
        offered.owner = 42;
        offered.tier = PackTier::Community;
        offered.visibility = Visibility::Draft;
        offered.fork_of = Some("Create".into());
        offered.tagline = "with the mod they wanted".into();

        let merged = target.with_authored_from(&offered);
        assert_eq!(merged.tagline, "with the mod they wanted");
        assert_eq!(merged.pack_id, "Create");
        assert_eq!(merged.owner, 1);
        assert_eq!(merged.tier, PackTier::Official);
        assert_eq!(merged.visibility, Visibility::Published);
        assert_eq!(merged.fork_of, None);
    }

    #[test]
    fn edit_rev_tracks_authored_content() {
        let base = config_with(vec![mr("jei.jar", "PROJ_JEI", "v1")]);
        let rev = base.edit_rev().unwrap();
        assert_eq!(
            rev,
            base.clone().edit_rev().unwrap(),
            "stable for one config"
        );

        let mut renamed = base.clone();
        renamed.mods[0].filename = "jei-1.19.jar".into();
        assert_ne!(rev, renamed.edit_rev().unwrap(), "a renamed row is an edit");

        let mut repinned = base.clone();
        repinned.mods[0].source = SourceDecl::Modrinth {
            project_id: "PROJ_JEI".into(),
            version_id: "v2".into(),
        };
        assert_ne!(rev, repinned.edit_rev().unwrap(), "a re-pin is an edit");

        let mut tagline = base.clone();
        tagline.tagline = "now with more gears".into();
        assert_ne!(rev, tagline.edit_rev().unwrap(), "a field edit is an edit");

        let mut added = base.clone();
        added.mods.push(mr("c.jar", "PROJ_C", "v1"));
        assert_ne!(rev, added.edit_rev().unwrap(), "an added mod is an edit");
    }

    // The other half of the rule: server-side housekeeping is not a conflict.
    // Publishing a pack, or a dependency fill appending a library, must not
    // reject the edit someone had in flight.
    #[test]
    fn edit_rev_ignores_server_controlled_fields_and_pulled_mods() {
        let base = config_with(vec![mr("jei.jar", "PROJ_JEI", "v1")]);
        let rev = base.edit_rev().unwrap();

        let mut published = base.clone();
        published.visibility = Visibility::Unlisted;
        published.tier = PackTier::Community;
        published.owner = 42;
        published.fork_of = Some("Industrial".into());
        assert_eq!(rev, published.edit_rev().unwrap(), "server fields excluded");

        let mut filled = base.clone();
        let mut dep = mr("redstoneflux.jar", "PROJ_RF", "v1");
        dep.pulled = true;
        filled.mods.push(dep);
        assert_eq!(rev, filled.edit_rev().unwrap(), "pulled deps excluded");
    }
}
