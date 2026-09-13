//! The wire pack manifest and its parts: what the launcher downloads to
//! reproduce a pack. Pure data; serialization shape is the public contract
//! shared with the launcher's `SmrtPackManifest`.

use super::side::PresenceClass;
use super::version::VersionChannel;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct PackManifest {
    pub schema_version: u32,
    pub pack_id: String,
    pub pack_version: String,
    /// Release channel of this build (Modrinth `version_type` vocabulary),
    /// stored -- the version string carries no channel semantics. Absent on
    /// manifests built before the field landed; readers fall back to the
    /// legacy string rule (`SNAPSHOT-` prefix = beta). Additive, so the
    /// schema version stays at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel: Option<VersionChannel>,
    /// Curator-authored release notes for this build (the Modrinth
    /// `version.changelog` analog). CommonMark; absent when none were given.
    /// The structural diff endpoint complements it -- this is the "why", the
    /// diff is the "what".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub changelog: Option<String>,
    /// The same notes per language, keyed by a language tag (`en`, `ru`).
    ///
    /// `changelog` stays what a client reads when it can match nothing here, so
    /// every existing client keeps working and nothing has to guess which
    /// language an untagged string is written in -- the tagged map includes that
    /// language too, and the untagged copy is the compatibility shim. Open by
    /// design: a mirror serving a community that speaks neither of the panel's
    /// own languages is the ordinary case for a self-hosted registry, not an
    /// exception. Additive, so the schema version stays at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub changelog_i18n: Option<std::collections::BTreeMap<String, String>>,
    pub generated_at: String,
    /// Content fingerprint: a stable hash of what actually lands in an instance
    /// (artifact hashes + install flags + loader/java/mc), independent of the
    /// `pack_version` label and `generated_at`. Two builds with identical
    /// content share a fingerprint; a launcher uses it as the reliable "did the
    /// content change?" signal rather than trusting a hand-assigned version
    /// bump. Additive (absent on manifests built before it landed; old clients
    /// ignore it), so the schema version stays at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fingerprint: Option<String>,
    /// What the mirror knew about this build when it published it. Absent when
    /// the pre-publish check found nothing to say (and on every manifest built
    /// before the check landed), so the schema version stays at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checks: Option<BuildChecks>,
    /// The commit this build was made from (#122), so a published build is
    /// reproducible: the config it came from is a stored snapshot rather than
    /// whatever happened to be on screen at that instant. Absent on builds made
    /// straight from the live config -- the CLI's `--config <file>`, and every
    /// build made before history landed -- so the schema version stays at 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub built_from: Option<String>,
    pub minecraft: MinecraftSpec,
    pub loader: LoaderSpec,
    pub java: JavaSpec,
    /// See [`AuthSpec`]. Absent = no auth precondition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth: Option<AuthSpec>,
    pub mods: Vec<ModEntry>,
    #[serde(default)]
    pub assets: Vec<AssetEntry>,
}

/// The pre-publish check, as the published build records it.
///
/// A build is resolved against the registry graph before it is written. Two
/// findings mean the pack cannot start -- a declared hard dependency nothing
/// satisfies, and an artifact no loader in the pack can run -- and those stop
/// the publish. The rest are recorded rather than enforced: an active conflict may be
/// deliberate, a version outside a declared window usually runs anyway, and an
/// unidentified jar means the check was partial rather than clean.
///
/// Recorded on the manifest because that is the artifact of a build; a job log
/// is in memory and its snapshot outlives nothing. Advisory to a launcher --
/// nothing here changes what gets installed.
///
/// On a published manifest, a non-empty `blocking` means the build went out
/// over the gate, and `overridden` says so explicitly; a preview manifest
/// carries the same findings unoverridden, since nothing was published. Who
/// overrode it is not here: the actor lives in the job log and the audit trail,
/// which are behind the panel, and this file is public.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct BuildChecks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisory: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub overridden: bool,
}

impl BuildChecks {
    /// Nothing worth recording -- the manifest omits the block entirely rather
    /// than carrying an empty one.
    pub fn is_empty(&self) -> bool {
        self.blocking.is_empty() && self.advisory.is_empty()
    }
}

/// Auth precondition the launcher enforces before spawning the game: which
/// provider the user must be signed in with, and (for SmartyCraft-bound
/// content) which SC game server the join + session bind to. Optional and
/// additive -- a manifest without it launches with no auth precondition.
/// `kind` is an open vocabulary the launcher matches (`smartycraft`,
/// `microsoft`, `both` today); unknown kinds are advisory, never blocking.
#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct AuthSpec {
    pub kind: String,
    /// The SC game-server id the join binds to (e.g. `Industrial`); absent
    /// for providers with no SC join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct MinecraftSpec {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct LoaderSpec {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct JavaSpec {
    pub major: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ModEntry {
    pub filename: String,
    pub sha1: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    #[serde(default = "default_true")]
    pub required: bool,
    /// Install-time default for an optional entry (`required = false`): on unless
    /// a curator opts it out. Omitted from the wire when true (the launcher's
    /// SmrtModEntry defaults it to true), so only an opted-out mod carries it.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub default_enabled: bool,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<Display>,
    /// Curator-assigned stable identity, independent of the versioned filename. The
    /// launcher keys an optional mod's on/off state by it so the choice survives a
    /// version bump (ADR 0002); a Modrinth mod already has its project id, so this
    /// carries the stable key for a self-hosted (smrt_cache) mod. Absent when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct AssetEntry {
    pub dest: String,
    pub sha1: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    #[serde(default = "default_true")]
    pub required: bool,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<Display>,
}

/// Advisory display metadata for launcher UIs. Adding or removing this
/// block on an existing manifest is forward-compatible -- the wire schema
/// version stays at 2. Clients that don't recognise the block fall back
/// to defaults derived from `filename` / `dest`.
///
/// `icon_url` and `requires` are additive launcher-side richer UX hooks
/// (per-item icons, dependency graph rendering). Both optional; manifests
/// without them parse cleanly on every client that reached the v2 schema.
///
/// A `role` field once grouped interchangeable mods into one selectable slot.
/// It was only ever settable through a hand-written TOML table and a CLI pass,
/// which is more curation than the grouping was worth; a mod that must not run
/// beside another says so through `incompatible_with`, which is the part that
/// protects an install. A manifest still carrying the key parses -- an unknown
/// field is ignored -- and a rebuild drops it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct Display {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incompatible_with: Vec<String>,
    /// SPDX license identifier where known (e.g. "MIT", "LGPL-3.0-only",
    /// "CC-BY-NC-SA-3.0"). Useful for a launcher to surface
    /// non-redistributable mods to the user. Absent for proprietary mods
    /// without an SPDX-compatible declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub license: Option<String>,
    /// Source / project / wiki URL. Used by a launcher's "Learn more"
    /// affordance. Preferred order: mcmod.info url, Modrinth source_url,
    /// CurseForge project page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    /// Per-item icon URL. Mirror serves directly for smrt_cache /
    /// smrt_static entries; Modrinth-sourced entries can leave this null
    /// and let the client resolve via the source's `project_id` against
    /// the Modrinth API. Null = client falls back to a letter avatar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_url: Option<String>,
    /// Same-manifest dependency declarations. Each entry's `filename`
    /// points at another mod in this pack's `mods[]`. Resolver
    /// validates the reference at install time; missing references
    /// surface as a warning rather than a hard failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
    /// Presence class of the entry in this pack (required / optional_client /
    /// optional_server / optional_both / coremod), computed at build from the
    /// side+policy classification and the dependency graph. Advisory like the
    /// rest of the block: `ModEntry.required` stays the enforcing flag, and the
    /// two are consistent by construction. Absent = unclassified (and on every
    /// manifest built before the field landed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub presence: Option<PresenceClass>,
}

/// Single edge in a mod's dependency DAG. [filename] must match a
/// mods[] entry's filename in the same manifest. [version_range]
/// follows Maven-style range syntax (`>=4.0`, `[1.0,2.0)`); null
/// means "any version present is acceptable". [optional] = true
/// means the consumer works without the dep but works better with
/// it -- launcher shows it greyed-out in the dep tree.
#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct Requirement {
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version_range: Option<String>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Modrinth {
        project_id: String,
        version_id: String,
    },
    /// A published CurseForge file, resolved at build time.
    ///
    /// The ids are kept beside the url because they are the provenance: they say
    /// whose file this is in a form that survives the link expiring, and they are
    /// what the mirror would re-resolve against. `url` is absent exactly when the
    /// project's author has disabled third-party distribution, and a launcher
    /// meeting that has to send the player to the project page rather than
    /// fetching anything.
    #[serde(rename = "curseforge")]
    CurseForge {
        #[ts(type = "number")]
        project_id: i64,
        #[ts(type = "number")]
        file_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    SmrtCache {
        url: String,
    },
    SmrtStatic {
        url: String,
    },
}

fn default_true() -> bool {
    true
}

/// `skip_serializing_if` for bool-default-true fields: keeps the manifest clean
/// by omitting the field when it holds its default (e.g. `default_enabled`).
fn is_true(value: &bool) -> bool {
    *value
}

/// `skip_serializing_if` for bool-default-false fields.
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_entry_serializes_without_display_block_when_absent() {
        let m = ModEntry {
            filename: "Quark.jar".into(),
            sha1: "abc".into(),
            size_bytes: 100,
            required: true,
            default_enabled: true,
            source: Source::SmrtCache { url: "u".into() },
            display: None,
            slug: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(
            !s.contains("display"),
            "absent display block must not serialize (forward-compat for old clients): {s}"
        );
    }

    #[test]
    fn mod_entry_round_trips_display_block() {
        let json = r#"{
            "filename": "VoxelMap.jar",
            "sha1": "deadbeef",
            "size_bytes": 1024,
            "required": false,
            "source": {"type": "smrt_cache", "url": "https://example/v1/cache/de/deadbeef.jar"},
            "display": {
                "name": "VoxelMap",
                "description": "Minimap with waypoints",
                "category": "minimap",
                "incompatible_with": ["XaerosMinimap.jar", "JourneyMap.jar"]
            }
        }"#;
        let m: ModEntry = serde_json::from_str(json).unwrap();
        let d = m.display.expect("display deserialized");
        assert_eq!(d.name.as_deref(), Some("VoxelMap"));
        assert_eq!(d.category.as_deref(), Some("minimap"));
        assert_eq!(
            d.incompatible_with,
            vec!["XaerosMinimap.jar", "JourneyMap.jar"]
        );
    }

    #[test]
    fn mod_entry_round_trips_rich_display_block() {
        // icon_url + requires together. Wire format is what the
        // 2026-05-25 launcher spec extension expects; this test fails
        // loud if a field name drifts. The `role` key rides along as a
        // field no reader knows any more: an old manifest carrying it must
        // still parse, which is what "ignore unknown fields" is for.
        let json = r#"{
            "filename": "appleskin.jar",
            "sha1": "abc",
            "size_bytes": 50000,
            "required": true,
            "source": {"type": "modrinth", "project_id": "EsAfCjCV", "version_id": "v"},
            "display": {
                "name": "AppleSkin",
                "category": "performance",
                "icon_url": "https://cdn.modrinth.com/data/EsAfCjCV/icon.png",
                "role": "info_overlay",
                "requires": [
                    {"filename": "Mixinbooter.jar", "version_range": ">=10.0", "optional": false}
                ]
            }
        }"#;
        let m: ModEntry = serde_json::from_str(json).unwrap();
        let d = m.display.expect("display deserialized");
        assert_eq!(
            d.icon_url.as_deref(),
            Some("https://cdn.modrinth.com/data/EsAfCjCV/icon.png")
        );
        assert_eq!(d.requires.len(), 1);
        assert_eq!(d.requires[0].filename, "Mixinbooter.jar");
        assert_eq!(d.requires[0].version_range.as_deref(), Some(">=10.0"));
        assert!(!d.requires[0].optional);
    }

    // Every published manifest and pack config written before the role field
    // was dropped still carries `"role": "..."`. Nothing may reject it: an
    // unknown field is ignored, and a rebuild simply stops emitting it.
    #[test]
    fn a_manifest_carrying_the_dropped_role_key_still_parses() {
        let json = r#"{
            "filename": "JEI.jar",
            "sha1": "abc",
            "size_bytes": 1,
            "required": false,
            "source": {"type": "smrt_cache", "url": "u"},
            "display": {"name": "JEI", "role": "recipe_viewer", "category": "utility"}
        }"#;
        let m: ModEntry = serde_json::from_str(json).unwrap();
        let d = m.display.expect("display parsed");
        assert_eq!(d.category.as_deref(), Some("utility"));
        let s = serde_json::to_string(&d).unwrap();
        assert!(!s.contains("role"), "and a rebuild drops it: {s}");
    }

    #[test]
    fn requirement_optional_defaults_to_false_when_absent() {
        // version_range null AND optional missing -- a curator who
        // doesn't care about pinning a version should be able to write
        // `{"filename": "X.jar"}` and have it round-trip.
        let json = r#"{"filename": "AppliedEnergistics2.jar"}"#;
        let r: Requirement = serde_json::from_str(json).unwrap();
        assert_eq!(r.filename, "AppliedEnergistics2.jar");
        assert_eq!(r.version_range, None);
        assert!(!r.optional);
        // Round-trip preserves the omission shape.
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            !s.contains("version_range"),
            "absent version_range must not serialize: {s}"
        );
        assert!(
            s.contains("\"optional\":false"),
            "optional always serializes (no skip_if): {s}"
        );
    }

    #[test]
    fn display_with_empty_requires_does_not_emit_field() {
        // Vec::is_empty skip is critical -- otherwise every existing
        // manifest entry would gain a noisy `"requires":[]` on next
        // build, churning every cache + breaking byte-equality
        // comparisons in client-side change detection.
        let d = Display {
            name: Some("X".into()),
            ..Display::default()
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(
            !s.contains("requires"),
            "empty requires must not serialize: {s}"
        );
        assert!(
            !s.contains("presence"),
            "absent presence must not serialize: {s}"
        );
    }

    #[test]
    fn display_presence_round_trips() {
        let json = r#"{"name": "Sodium (fork)", "presence": "optional_client"}"#;
        let d: Display = serde_json::from_str(json).unwrap();
        assert_eq!(d.presence, Some(PresenceClass::OptionalClient));
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"presence\":\"optional_client\""), "got {s}");
    }

    #[test]
    fn manifest_without_display_blocks_still_parses() {
        let json = r#"{
            "schema_version": 2,
            "pack_id": "Old",
            "pack_version": "2026.05.22",
            "generated_at": "2026-05-22T00:00:00Z",
            "minecraft": {"version": "1.12.2"},
            "loader": {"name": "forge", "version": "14.23.5.2922"},
            "java": {"major": 8},
            "mods": [{"filename": "X.jar", "sha1": "a", "size_bytes": 1, "required": true,
                "source": {"type": "smrt_cache", "url": "u"}}]
        }"#;
        let pm: PackManifest = serde_json::from_str(json).unwrap();
        assert!(
            pm.mods[0].display.is_none(),
            "manifests written before the display field landed must still parse"
        );
        assert!(
            pm.fingerprint.is_none(),
            "a manifest from before the fingerprint field must parse with None"
        );
    }

    #[test]
    fn auth_block_round_trips_and_is_omitted_when_absent() {
        let json = r#"{
            "schema_version": 2,
            "pack_id": "Create",
            "pack_version": "0.1.9",
            "generated_at": "2026-07-19T00:00:00Z",
            "minecraft": {"version": "1.21.1"},
            "loader": {"name": "neoforge", "version": "21.1.241"},
            "java": {"major": 21},
            "auth": {"kind": "smartycraft", "server_id": "Create"},
            "mods": []
        }"#;
        let pm: PackManifest = serde_json::from_str(json).unwrap();
        let auth = pm.auth.as_ref().expect("auth block parsed");
        assert_eq!(auth.kind, "smartycraft");
        assert_eq!(auth.server_id.as_deref(), Some("Create"));

        // absent on manifests that predate the field, and never serialized as null
        let bare = PackManifest { auth: None, ..pm };
        let s = serde_json::to_string(&bare).unwrap();
        assert!(!s.contains("\"auth\""), "absent auth must omit: {s}");
    }

    #[test]
    fn fingerprint_round_trips_and_is_omitted_when_absent() {
        let json = r#"{
            "schema_version": 2,
            "pack_id": "Industrial",
            "pack_version": "2026.06.06",
            "generated_at": "2026-06-06T00:00:00Z",
            "fingerprint": "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "minecraft": {"version": "1.12.2"},
            "loader": {"name": "forge", "version": "14.23.5.2922"},
            "java": {"major": 8},
            "mods": []
        }"#;
        let pm: PackManifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            pm.fingerprint.as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );

        // a None fingerprint must not serialize (forward-compat; no churn)
        let bare = PackManifest {
            fingerprint: None,
            ..pm
        };
        let s = serde_json::to_string(&bare).unwrap();
        assert!(
            !s.contains("fingerprint"),
            "absent fingerprint must omit: {s}"
        );
    }
}
