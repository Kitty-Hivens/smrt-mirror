//! Structured change summary between two builds of a pack -- what a launcher's
//! update dialog renders instead of guessing from file lists. Pure compute
//! over two wire manifests; version-label enrichment (sha1 -> version string)
//! is layered on by the HTTP handler, which has the registry.

use super::manifest::{ModEntry, PackManifest, Source};
use serde::Serialize;
use ts_rs::TS;
use utoipa::ToSchema;

/// A scalar that changed between the two builds, verbatim.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct FieldChange {
    pub from: String,
    pub to: String,
}

/// One entry present on only one side of the diff.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct DiffEntry {
    pub filename: String,
    /// Version label where the registry knows the artifact; the handler fills
    /// it, the pure diff leaves it absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
}

/// One entry present on both sides whose artifact changed.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct DiffUpdate {
    /// The `to` side's filename (the one an updated instance ends up with).
    pub filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version_to: Option<String>,
    pub sha1_from: String,
    pub sha1_to: String,
}

/// An entry whose install-time default flipped (content unchanged).
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct DiffToggle {
    pub filename: String,
    pub default_enabled_from: bool,
    pub default_enabled_to: bool,
}

/// The change summary between two builds, `from` -> `to`. Entries are matched
/// by stable identity (Modrinth project, else the curator slug, else the
/// filename), so a version bump that renames the jar still reads as an update
/// rather than a remove + add.
#[derive(Debug, Clone, Serialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct PackDiff {
    pub schema_version: u32,
    pub pack_id: String,
    pub from: String,
    pub to: String,
    /// False when the two builds share a content fingerprint (a relabel).
    pub content_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub loader: Option<FieldChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub minecraft: Option<FieldChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub java: Option<FieldChange>,
    pub mods_added: Vec<DiffEntry>,
    pub mods_removed: Vec<DiffEntry>,
    pub mods_updated: Vec<DiffUpdate>,
    pub mods_toggled: Vec<DiffToggle>,
    pub assets_added: Vec<DiffEntry>,
    pub assets_removed: Vec<DiffEntry>,
    pub assets_updated: Vec<DiffUpdate>,
}

/// The identity a mod entry is matched by across builds: the Modrinth project
/// (a re-pin is the same mod), else the curator slug (ADR 0002), else the
/// filename.
fn identity(m: &ModEntry) -> String {
    match &m.source {
        Source::Modrinth { project_id, .. } => format!("m:{project_id}"),
        _ => match &m.slug {
            Some(s) => format!("s:{s}"),
            None => format!("f:{}", m.filename),
        },
    }
}

/// Pure structural diff of two manifests. Order within each bucket follows the
/// `to` side (added/updated) or the `from` side (removed), which is already
/// filename-sorted by the build.
pub fn diff_manifests(from: &PackManifest, to: &PackManifest) -> PackDiff {
    let field = |a: &str, b: &str| -> Option<FieldChange> {
        (a != b).then(|| FieldChange {
            from: a.to_string(),
            to: b.to_string(),
        })
    };

    let from_by_id: std::collections::HashMap<String, &ModEntry> =
        from.mods.iter().map(|m| (identity(m), m)).collect();
    let to_ids: std::collections::HashSet<String> = to.mods.iter().map(identity).collect();

    let mut mods_added = Vec::new();
    let mut mods_updated = Vec::new();
    let mut mods_toggled = Vec::new();
    for m in &to.mods {
        match from_by_id.get(&identity(m)) {
            None => mods_added.push(DiffEntry {
                filename: m.filename.clone(),
                version: None,
            }),
            // Both, independently: a re-pin and a flipped install default are
            // two different things to tell somebody, and a mod that did both
            // used to report only the re-pin -- so an update dialog said
            // "updated" and never mentioned the mod had also stopped being
            // installed by default.
            Some(old) => {
                if old.sha1 != m.sha1 {
                    mods_updated.push(DiffUpdate {
                        filename: m.filename.clone(),
                        version_from: None,
                        version_to: None,
                        sha1_from: old.sha1.clone(),
                        sha1_to: m.sha1.clone(),
                    });
                }
                if old.default_enabled != m.default_enabled {
                    mods_toggled.push(DiffToggle {
                        filename: m.filename.clone(),
                        default_enabled_from: old.default_enabled,
                        default_enabled_to: m.default_enabled,
                    });
                }
            }
        }
    }
    let mods_removed = from
        .mods
        .iter()
        .filter(|m| !to_ids.contains(&identity(m)))
        .map(|m| DiffEntry {
            filename: m.filename.clone(),
            version: None,
        })
        .collect();

    // assets have no cross-build identity beyond their destination path
    let from_assets: std::collections::HashMap<&str, &super::manifest::AssetEntry> =
        from.assets.iter().map(|a| (a.dest.as_str(), a)).collect();
    let to_dests: std::collections::HashSet<&str> =
        to.assets.iter().map(|a| a.dest.as_str()).collect();
    let mut assets_added = Vec::new();
    let mut assets_updated = Vec::new();
    for a in &to.assets {
        match from_assets.get(a.dest.as_str()) {
            None => assets_added.push(DiffEntry {
                filename: a.dest.clone(),
                version: None,
            }),
            Some(old) if old.sha1 != a.sha1 => assets_updated.push(DiffUpdate {
                filename: a.dest.clone(),
                version_from: None,
                version_to: None,
                sha1_from: old.sha1.clone(),
                sha1_to: a.sha1.clone(),
            }),
            Some(_) => {}
        }
    }
    let assets_removed = from
        .assets
        .iter()
        .filter(|a| !to_dests.contains(a.dest.as_str()))
        .map(|a| DiffEntry {
            filename: a.dest.clone(),
            version: None,
        })
        .collect();

    PackDiff {
        schema_version: super::manifest::SCHEMA_VERSION,
        pack_id: to.pack_id.clone(),
        from: from.pack_version.clone(),
        to: to.pack_version.clone(),
        content_changed: match (&from.fingerprint, &to.fingerprint) {
            (Some(a), Some(b)) => a != b,
            _ => true, // a pre-fingerprint build: assume changed
        },
        loader: field(&from.loader.version, &to.loader.version).map(|c| FieldChange {
            from: format!("{} {}", from.loader.name, c.from),
            to: format!("{} {}", to.loader.name, c.to),
        }),
        minecraft: field(&from.minecraft.version, &to.minecraft.version),
        java: field(&from.java.major.to_string(), &to.java.major.to_string()),
        mods_added,
        mods_removed,
        mods_updated,
        mods_toggled,
        assets_added,
        assets_removed,
        assets_updated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::manifest::{
        AssetEntry, JavaSpec, LoaderSpec, MinecraftSpec, PackManifest, SCHEMA_VERSION,
    };

    fn entry(filename: &str, sha1: &str, project: Option<&str>, enabled: bool) -> ModEntry {
        ModEntry {
            filename: filename.into(),
            sha1: sha1.into(),
            size_bytes: 1,
            required: false,
            default_enabled: enabled,
            source: match project {
                Some(p) => Source::Modrinth {
                    project_id: p.into(),
                    version_id: "v".into(),
                },
                None => Source::SmrtCache { url: "u".into() },
            },
            display: None,
            slug: None,
        }
    }

    fn manifest(version: &str, loader: &str, mods: Vec<ModEntry>) -> PackManifest {
        PackManifest {
            schema_version: SCHEMA_VERSION,
            pack_id: "Create".into(),
            pack_version: version.into(),
            channel: None,
            changelog: None,
            changelog_i18n: None,
            generated_at: "T".into(),
            fingerprint: Some(format!("fp-{version}-{loader}")),
            checks: None,
            built_from: None,
            minecraft: MinecraftSpec {
                version: "1.21.1".into(),
            },
            loader: LoaderSpec {
                name: "neoforge".into(),
                version: loader.into(),
            },
            java: JavaSpec { major: 21 },
            auth: None,
            mods,
            assets: vec![AssetEntry {
                dest: "resourcepacks/FreshAnimations.zip".into(),
                sha1: "aaa".into(),
                size_bytes: 1,
                required: false,
                source: Source::SmrtStatic { url: "u".into() },
                display: None,
            }],
        }
    }

    #[test]
    fn diff_reads_as_an_update_dialog_would() {
        // 0.1.2: old loader, CSL enabled, old sodium pin, an extra mod
        let from = manifest(
            "0.1.2",
            "21.1.186",
            vec![
                entry(
                    "CustomSkinLoader_Universal-15.0.1.jar",
                    "c1",
                    Some("csl"),
                    true,
                ),
                entry("sodium-0.6.0.jar", "s1", Some("sodium"), true),
                entry("gone.jar", "g1", None, true),
            ],
        );
        // 0.1.4: new loader, CSL default-off, sodium re-pinned (renamed jar), WTHIT added
        let to = manifest(
            "0.1.4",
            "21.1.241",
            vec![
                entry(
                    "CustomSkinLoader_Universal-15.0.1.jar",
                    "c1",
                    Some("csl"),
                    false,
                ),
                entry("sodium-0.6.13.jar", "s2", Some("sodium"), true),
                entry("WTHIT.jar", "w1", Some("wthit"), false),
            ],
        );
        let d = diff_manifests(&from, &to);
        assert_eq!((d.from.as_str(), d.to.as_str()), ("0.1.2", "0.1.4"));
        assert!(d.content_changed);
        assert_eq!(
            d.loader.as_ref().map(|c| (c.from.as_str(), c.to.as_str())),
            Some(("neoforge 21.1.186", "neoforge 21.1.241"))
        );
        assert!(d.minecraft.is_none(), "unchanged scalars stay absent");
        // the sodium re-pin renamed the jar but matches by project identity
        assert_eq!(d.mods_updated.len(), 1);
        assert_eq!(d.mods_updated[0].filename, "sodium-0.6.13.jar");
        assert_eq!(
            (
                d.mods_updated[0].sha1_from.as_str(),
                d.mods_updated[0].sha1_to.as_str()
            ),
            ("s1", "s2")
        );
        assert_eq!(d.mods_added.len(), 1);
        assert_eq!(d.mods_added[0].filename, "WTHIT.jar");
        assert_eq!(d.mods_removed.len(), 1);
        assert_eq!(d.mods_removed[0].filename, "gone.jar");
        assert_eq!(d.mods_toggled.len(), 1);
        assert_eq!(
            d.mods_toggled[0].filename,
            "CustomSkinLoader_Universal-15.0.1.jar"
        );
        assert!(!d.mods_toggled[0].default_enabled_to);
        // identical assets on both sides -> empty buckets
        assert!(d.assets_added.is_empty() && d.assets_removed.is_empty());
    }

    // A re-pin and a flipped install default are two different things to tell
    // somebody. A mod that did both used to report only the re-pin, so the
    // update dialog said "updated" and never mentioned it had stopped being
    // installed by default.
    #[test]
    fn a_mod_that_was_repinned_and_toggled_reports_both() {
        let a = manifest(
            "0.1.2",
            "21.1.186",
            vec![entry("sodium.jar", "s1", Some("AANobbMI"), true)],
        );
        let b = manifest(
            "0.1.3",
            "21.1.186",
            vec![entry("sodium.jar", "s2", Some("AANobbMI"), false)],
        );
        let d = diff_manifests(&a, &b);
        assert_eq!(d.mods_updated.len(), 1, "the re-pin");
        assert_eq!(d.mods_toggled.len(), 1, "and the toggle");
        assert!(!d.mods_toggled[0].default_enabled_to);
        assert!(d.mods_added.is_empty() && d.mods_removed.is_empty());
    }

    #[test]
    fn a_relabel_diff_is_empty_and_says_so() {
        let a = manifest("0.1.2", "21.1.186", vec![entry("x.jar", "x1", None, true)]);
        let mut b = manifest("0.1.3", "21.1.186", vec![entry("x.jar", "x1", None, true)]);
        b.fingerprint = a.fingerprint.clone();
        let d = diff_manifests(&a, &b);
        assert!(!d.content_changed, "same fingerprint = a relabel");
        assert!(
            d.mods_added.is_empty()
                && d.mods_removed.is_empty()
                && d.mods_updated.is_empty()
                && d.mods_toggled.is_empty()
        );
    }
}
