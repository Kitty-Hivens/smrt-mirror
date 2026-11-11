//! Modern declared mod metadata: `META-INF/mods.toml` /
//! `META-INF/neoforge.mods.toml` (Forge/NeoForge) and `fabric.mod.json`
//! (Fabric/Quilt). Unlike 1.12.2 `mcmod.info` (sparse, untyped -- see
//! [`super::curator::read_mcmod_info`]), these carry a mod's identity plus typed,
//! version-ranged dependencies, so a self-hosted modern jar that is not on
//! Modrinth still gets a real identity and dependency graph.
//!
//! Best-effort: an unreadable jar or unparseable file yields an empty [`ModMeta`],
//! never an error.

use super::archive::read_zip_entry;
use crate::registry::model::{RelKind, Severity};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Cursor;

/// Modid values that name the platform, not a real mod dependency.
const PLATFORM_MODIDS: &[&str] = &[
    "minecraft",
    "java",
    "forge",
    "neoforge",
    "fabricloader",
    "fabric-loader",
    "quilt_loader",
    "quilt_base",
    "mcp",
    "fml",
    "javafml",
    "lowcodefml",
];

/// The loader a platform modid names, for the ones that mean the loader build
/// itself. `javafml`/`fml` are language-loader versions and name no build, so
/// they are absent; so is `minecraft`, which the `mc` field carries.
fn loader_of_platform_modid(modid: &str) -> Option<&'static str> {
    Some(match modid.to_ascii_lowercase().as_str() {
        "forge" => "forge",
        "neoforge" => "neoforge",
        "fabricloader" | "fabric-loader" => "fabric",
        "quilt_loader" => "quilt",
        _ => return None,
    })
}

/// A declared dependency: the target modid, its kind, an optional version
/// range (Maven-style for Forge, semver-ish for Fabric), and the side the
/// dependency is needed on (`BOTH`/`CLIENT`/`SERVER`, Forge/NeoForge only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredDep {
    pub modid: String,
    pub kind: RelKind,
    /// How hard an incompatibility is, straight from the declaration. `None`
    /// for every other kind -- a requirement has no intensity (#129).
    pub severity: Option<Severity>,
    pub version_range: Option<String>,
    pub side: Option<String>,
}

/// The window a jar declares on the loader build it runs under -- JEI's
/// `neoforge [21.1.238,)`. Not a mod edge: the loader is always present, so
/// this is a property of the artifact, like the loaders it targets. Kept apart
/// from [`ModMeta::deps`] for exactly that reason (#164).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderReq {
    /// The loader as this mirror names it (`forge` / `neoforge` / `fabric` /
    /// `quilt`), not the modid spelling.
    pub loader: String,
    /// The declared range, verbatim (Maven for Forge/NeoForge, semver-ish for
    /// Fabric). Never the "any version" wildcard -- that is dropped.
    pub range: String,
}

/// A jar's modern declared metadata.
#[derive(Debug, Clone, Default)]
pub struct ModMeta {
    /// The mod's own id, when declared. Fallback identity for a jar without an
    /// `mcmod.info` and without a Modrinth match.
    pub modid: Option<String>,
    /// Declared human-readable name (`displayName` / fabric `name`).
    pub display_name: Option<String>,
    /// Declared version, verbatim -- may be a gradle placeholder like
    /// `${file.jarVersion}`; the jar reader resolves that against the
    /// MANIFEST.MF `Implementation-Version` before use.
    pub version: Option<String>,
    /// Declared icon path inside the jar (`logoFile` / fabric `icon`),
    /// normalized to no leading slash.
    pub logo_file: Option<String>,
    /// The Minecraft version the jar targets, from its `minecraft` dependency
    /// range (the first concrete version in it). Used by the upload gate to check
    /// Modrinth coverage; `None` when no usable version is declared.
    pub mc: Option<String>,
    pub deps: Vec<DeclaredDep>,
    /// Windows the jar declares on the loader build itself. Usually one; a jar
    /// naming several loaders declares one apiece.
    pub loader_reqs: Vec<LoaderReq>,
    /// `fabric.mod.json` `environment` verbatim (`*` | `client` | `server`).
    pub environment: Option<String>,
    /// Whether `entrypoints.client` / `entrypoints.main`+`entrypoints.server`
    /// are declared (Fabric): a client entrypoint with no main/server one is a
    /// client-side shape.
    pub client_entrypoint: bool,
    pub main_entrypoint: bool,
    /// `mods.toml` / `neoforge.mods.toml` `displayTest` verbatim
    /// (MATCH_VERSION | IGNORE_SERVER_VERSION | IGNORE_ALL_VERSION | NONE).
    /// Anything but the default MATCH_VERSION means the mod tolerates a server
    /// that lacks it -- the modern spelling of `acceptableRemoteVersions="*"`.
    pub display_test: Option<String>,
    /// The language loader a `mods.toml` names in `modLoader`, when a separate
    /// mod has to provide it (`klf` for KotlinLangForge, `kotlinforforge` for
    /// Kotlin for Forge). `None` for the two that ship with the loader,
    /// `javafml` and `lowcodefml`, and for a jar that names none. The loader
    /// refuses to load the mod without its provider, so this is a dependency
    /// no metadata can understate.
    pub language_loader: Option<String>,
}

impl ModMeta {
    /// True when the declared `displayTest` marks the mod tolerant of a
    /// mismatched/absent remote side.
    pub fn display_test_tolerant(&self) -> bool {
        matches!(
            self.display_test.as_deref(),
            Some("IGNORE_SERVER_VERSION") | Some("IGNORE_ALL_VERSION") | Some("NONE")
        )
    }
}

/// Read a jar's modern metadata: try the Forge/NeoForge TOML first, then Fabric
/// JSON. Empty when neither is present or parseable.
pub fn read_mod_meta(jar_bytes: &[u8]) -> ModMeta {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(jar_bytes)) else {
        return ModMeta::default();
    };
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(raw) = read_named(&mut zip, name)
            && let Ok(text) = std::str::from_utf8(&raw)
        {
            return parse_mods_toml(text);
        }
    }
    if let Some(raw) = read_named(&mut zip, "fabric.mod.json") {
        return parse_fabric_json(&raw);
    }
    ModMeta::default()
}

fn read_named(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let size = entry.size();
    read_zip_entry(&mut entry, size, name).ok()
}

// ── Forge / NeoForge mods.toml ───────────────────────────────────────────────

#[derive(Deserialize)]
struct ModsToml {
    #[serde(rename = "modLoader", default)]
    mod_loader: Option<String>,
    #[serde(default)]
    mods: Vec<ModsTomlMod>,
    /// Top-level `logoFile` -- the standard spelling; NeoForge also allows a
    /// per-mod one, which wins when both are present.
    #[serde(rename = "logoFile", default)]
    logo_file: Option<String>,
    /// `[[dependencies.<owner-modid>]]` -- keyed by the mod the deps belong to.
    #[serde(default)]
    dependencies: HashMap<String, Vec<ModsTomlDep>>,
}

#[derive(Deserialize)]
struct ModsTomlMod {
    #[serde(rename = "modId", alias = "modid", default)]
    mod_id: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "logoFile", default)]
    logo_file: Option<String>,
    #[serde(rename = "displayTest", default)]
    display_test: Option<String>,
}

#[derive(Deserialize)]
struct ModsTomlDep {
    #[serde(rename = "modId", alias = "modid", default)]
    mod_id: Option<String>,
    /// Legacy (1.13-1.18) required flag; superseded by `type`.
    #[serde(default)]
    mandatory: Option<bool>,
    /// Modern (1.19+/NeoForge): required|optional|incompatible|discouraged|embedded.
    #[serde(rename = "type", default)]
    dep_type: Option<String>,
    #[serde(rename = "versionRange", alias = "versionrange", default)]
    version_range: Option<String>,
    /// Which side needs the dependency: BOTH (default) | CLIENT | SERVER.
    #[serde(default)]
    side: Option<String>,
}

/// Parse a `mods.toml` / `neoforge.mods.toml` body.
pub fn parse_mods_toml(text: &str) -> ModMeta {
    let Ok(parsed) = toml::from_str::<ModsToml>(text) else {
        return ModMeta::default();
    };
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let language_loader = clean(parsed.mod_loader).filter(|l| !is_bundled_language(l));
    let mut modid = None;
    let mut display_name = None;
    let mut version = None;
    let mut logo_file = None;
    let mut display_test = None;
    for m in parsed.mods {
        if modid.is_none() {
            modid = m.mod_id.filter(|s| !s.trim().is_empty());
            display_name = clean(m.display_name);
            version = clean(m.version);
            logo_file = clean(m.logo_file);
            display_test = m
                .display_test
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty());
        }
    }
    let logo_file = logo_file
        .or_else(|| clean(parsed.logo_file))
        .map(|s| s.trim_start_matches('/').to_string());
    let mut deps = Vec::new();
    let mut loader_reqs = Vec::new();
    let mut mc = None;
    for entry in parsed.dependencies.into_values().flatten() {
        // read the kind (borrows entry) before consuming its fields
        let Some((kind, severity)) = forge_dep_kind(&entry) else {
            continue; // embedded / unknown -> not a graph edge
        };
        let Some(target) = entry
            .mod_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        // the minecraft dependency carries the target MC version, not a mod edge
        if target.eq_ignore_ascii_case("minecraft") {
            if mc.is_none() {
                mc = entry.version_range.as_deref().and_then(first_mc);
            }
            continue;
        }
        if is_platform_modid(&target) {
            // the loader is always present, so this is never a mod edge -- but
            // the window it names is the one a loader refuses to start outside
            if let Some(loader) = loader_of_platform_modid(&target)
                && let Some(range) = clean_range(entry.version_range)
            {
                loader_reqs.push(LoaderReq {
                    loader: loader.to_string(),
                    range,
                });
            }
            continue;
        }
        deps.push(DeclaredDep {
            modid: target,
            kind,
            severity,
            version_range: clean_range(entry.version_range),
            side: entry
                .side
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty()),
        });
    }
    ModMeta {
        modid,
        display_name,
        version,
        logo_file,
        mc,
        deps,
        loader_reqs,
        display_test,
        language_loader,
        ..ModMeta::default()
    }
}

/// The language loaders Forge and NeoForge ship themselves.
fn is_bundled_language(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "javafml" | "lowcodefml")
}

/// Kind of a Forge/NeoForge dependency: `type` when present, else the legacy
/// `mandatory` flag; a dep with neither qualifier reads as required.
fn forge_dep_kind(dep: &ModsTomlDep) -> Option<(RelKind, Option<Severity>)> {
    if let Some(t) = &dep.dep_type {
        return match t.to_ascii_lowercase().as_str() {
            "required" => Some((RelKind::Requires, None)),
            "optional" => Some((RelKind::OptionalDep, None)),
            // the loader refuses to start; `discouraged` is the author's advice
            "incompatible" => Some((RelKind::Conflicts, Some(Severity::Hard))),
            "discouraged" => Some((RelKind::Conflicts, Some(Severity::Soft))),
            _ => None, // embedded and anything unrecognised
        };
    }
    // the pre-`type` spelling: `mandatory` only ever said required or not
    match dep.mandatory {
        Some(false) => Some((RelKind::OptionalDep, None)),
        _ => Some((RelKind::Requires, None)),
    }
}

// ── Fabric / Quilt fabric.mod.json ───────────────────────────────────────────

#[derive(Deserialize)]
struct FabricModJson {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    /// A string path, or a `{ "<size>": "path" }` map from which any entry serves.
    #[serde(default)]
    icon: Option<serde_json::Value>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    entrypoints: HashMap<String, serde_json::Value>,
    #[serde(default)]
    depends: HashMap<String, VersionPredicate>,
    #[serde(default)]
    recommends: HashMap<String, VersionPredicate>,
    #[serde(default)]
    suggests: HashMap<String, VersionPredicate>,
    #[serde(default)]
    breaks: HashMap<String, VersionPredicate>,
    #[serde(default)]
    conflicts: HashMap<String, VersionPredicate>,
}

/// A Fabric version predicate is one string or an array of alternatives.
#[derive(Deserialize)]
#[serde(untagged)]
enum VersionPredicate {
    One(String),
    Many(Vec<String>),
}

impl VersionPredicate {
    /// A range string, or `None` for the "any version" wildcard `*`.
    fn range(&self) -> Option<String> {
        let joined = match self {
            VersionPredicate::One(s) => s.clone(),
            VersionPredicate::Many(v) => v.join(" || "),
        };
        clean_range(Some(joined))
    }
}

/// Parse a `fabric.mod.json` body. `depends` are required; `recommends` /
/// `suggests` optional; `breaks` a hard conflict; `conflicts` a soft one.
pub fn parse_fabric_json(bytes: &[u8]) -> ModMeta {
    let Ok(parsed) = serde_json::from_slice::<FabricModJson>(bytes) else {
        return ModMeta::default();
    };
    // the `minecraft` dependency carries the target MC version
    let mc = parsed
        .depends
        .get("minecraft")
        .and_then(|p| p.range())
        .as_deref()
        .and_then(first_mc);
    // `depends.fabricloader` is the loader build, not a mod: the same window
    // Forge writes as a `neoforge` dependency.
    let mut loader_reqs: Vec<LoaderReq> = parsed
        .depends
        .iter()
        .filter_map(|(modid, pred)| {
            Some(LoaderReq {
                loader: loader_of_platform_modid(modid)?.to_string(),
                range: pred.range()?,
            })
        })
        .collect();
    loader_reqs.sort_by(|a, b| a.loader.cmp(&b.loader));
    let mut deps = Vec::new();
    let mut add =
        |map: HashMap<String, VersionPredicate>, kind: RelKind, severity: Option<Severity>| {
            for (modid, pred) in map {
                if modid.trim().is_empty() || is_platform_modid(&modid) {
                    continue;
                }
                deps.push(DeclaredDep {
                    version_range: pred.range(),
                    modid,
                    kind,
                    severity,
                    side: None,
                });
            }
        };
    add(parsed.depends, RelKind::Requires, None);
    // `recommends` is the "works much better with" tier -- the Recommends
    // kind, surfaced to the curator as a suggestion, never auto-added;
    // `suggests` is weaker and stays a plain optional dependency.
    add(parsed.recommends, RelKind::Recommends, None);
    add(parsed.suggests, RelKind::OptionalDep, None);
    // Fabric's own words: `breaks` is the one that fails to load, `conflicts`
    // the one it merely warns about. Same two intensities, opposite names to
    // Forge's -- which is exactly why the intensity is its own field now.
    add(parsed.breaks, RelKind::Conflicts, Some(Severity::Hard));
    add(parsed.conflicts, RelKind::Conflicts, Some(Severity::Soft));
    deps.sort_by(|a, b| a.modid.cmp(&b.modid));
    let has_entry = |k: &str| {
        parsed
            .entrypoints
            .get(k)
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
    };
    let icon = parsed.icon.as_ref().and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => m.values().find_map(|x| x.as_str()).map(str::to_string),
        _ => None,
    });
    ModMeta {
        modid: parsed.id.filter(|s| !s.trim().is_empty()),
        display_name: parsed
            .name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        version: parsed
            .version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        logo_file: icon
            .map(|s| s.trim().trim_start_matches('/').to_string())
            .filter(|s| !s.is_empty()),
        mc,
        deps,
        loader_reqs,
        environment: parsed.environment.filter(|s| !s.trim().is_empty()),
        client_entrypoint: has_entry("client"),
        main_entrypoint: has_entry("main") || has_entry("server"),
        display_test: None,
        language_loader: None,
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn is_platform_modid(modid: &str) -> bool {
    let m = modid.to_ascii_lowercase();
    PLATFORM_MODIDS.contains(&m.as_str())
}

/// Normalise a declared range: drop the empty string and the `*` wildcard (both
/// mean "any version"), so only a real constraint becomes a `target_version_range`.
fn clean_range(range: Option<String>) -> Option<String> {
    range
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "*")
}

/// The first concrete Minecraft version in a dependency range -- the lower bound
/// of `[1.20.1,)` / `>=1.20.1` / a bare `1.19.2`. Requires a dotted version (so a
/// bare loader number like `47` is ignored). `None` when the range names none.
fn first_mc(range: &str) -> Option<String> {
    let bytes = range.as_bytes();
    let start = bytes.iter().position(u8::is_ascii_digit)?;
    let end = range[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or(range.len(), |i| start + i);
    let v = range[start..end].trim_end_matches('.');
    (v.contains('.')).then(|| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_neoforge_mods_toml_typed_deps() {
        let toml = r#"
            modLoader="javafml"
            loaderVersion="[47,)"
            [[mods]]
            modId="examplemod"
            version="1.0.0"

            [[dependencies.examplemod]]
            modId="jei"
            type="required"
            versionRange="[15.0.0,)"

            [[dependencies.examplemod]]
            modId="architectury"
            type="optional"

            [[dependencies.examplemod]]
            modId="badmod"
            type="incompatible"

            [[dependencies.examplemod]]
            modId="minecraft"
            type="required"
            versionRange="[1.20.1,)"

            [[dependencies.examplemod]]
            modId="somelib"
            type="embedded"
        "#;
        let m = parse_mods_toml(toml);
        assert_eq!(m.modid.as_deref(), Some("examplemod"));
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        let mut got: Vec<_> = m
            .deps
            .iter()
            .map(|d| (d.modid.as_str(), d.kind, d.version_range.as_deref()))
            .collect();
        got.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(
            got,
            vec![
                ("architectury", RelKind::OptionalDep, None),
                ("badmod", RelKind::Conflicts, None),
                ("jei", RelKind::Requires, Some("[15.0.0,)")),
                // minecraft filtered as a platform modid; somelib dropped (embedded)
            ]
        );
    }

    #[test]
    fn extracts_mc_from_minecraft_dependency() {
        // Forge: the minecraft dep's range lower bound, and it is not a mod edge
        let forge = parse_mods_toml(
            "[[mods]]\nmodId=\"m\"\n[[dependencies.m]]\nmodId=\"minecraft\"\ntype=\"required\"\nversionRange=\"[1.20.1,1.21)\"",
        );
        assert_eq!(forge.mc.as_deref(), Some("1.20.1"));
        assert!(forge.deps.iter().all(|d| d.modid != "minecraft"));

        // Fabric: from depends.minecraft
        let fabric = parse_fabric_json(br#"{"id":"m","depends":{"minecraft":">=1.19.2"}}"#);
        assert_eq!(fabric.mc.as_deref(), Some("1.19.2"));

        // a bare loader number (no dot) is not a version
        assert_eq!(first_mc("[47,)"), None);
        assert_eq!(first_mc(">=1.20.1"), Some("1.20.1".into()));
    }

    // The crash this exists for: JEI declares `neoforge [21.1.238,)`, a pack
    // pinned 21.1.234, and the loader stopped before the main menu naming JEI.
    // The window is read off the same dependency block that is filtered out of
    // the mod graph -- the loader is present, so it is never a missing mod, but
    // the build it names is not a mod fact at all.
    #[test]
    fn a_loader_dependency_is_a_window_and_never_a_mod_edge() {
        let toml = r#"
            [[mods]]
            modId="jei"
            [[dependencies.jei]]
            modId="neoforge"
            type="required"
            versionRange="[21.1.238,)"
            [[dependencies.jei]]
            modId="minecraft"
            type="required"
            versionRange="[1.21.1,1.21.2)"
            [[dependencies.jei]]
            modId="javafml"
            type="required"
            versionRange="[4,)"
        "#;
        let m = parse_mods_toml(toml);
        assert_eq!(
            m.loader_reqs,
            vec![LoaderReq {
                loader: "neoforge".into(),
                range: "[21.1.238,)".into(),
            }]
        );
        assert!(
            m.deps.is_empty(),
            "the loader is not a dependency to satisfy: {:?}",
            m.deps
        );
        assert_eq!(m.mc.as_deref(), Some("1.21.1"), "minecraft stays its own");

        // an open window says nothing, so it is not stored as a constraint
        let open = parse_mods_toml(
            "[[mods]]\nmodId=\"m\"\n[[dependencies.m]]\nmodId=\"forge\"\nversionRange=\"*\"",
        );
        assert!(open.loader_reqs.is_empty());
        let none = parse_mods_toml("[[mods]]\nmodId=\"m\"\n[[dependencies.m]]\nmodId=\"forge\"");
        assert!(none.loader_reqs.is_empty());
    }

    // Fabric writes the same fact as a `fabricloader` dependency, and the
    // loader name in the window is this mirror's, not the modid spelling.
    #[test]
    fn fabric_declares_its_loader_window_under_another_name() {
        let m = parse_fabric_json(
            br#"{"id":"m","depends":{"fabricloader":">=0.16.9","fabric-api":"*","minecraft":">=1.21"}}"#,
        );
        assert_eq!(
            m.loader_reqs,
            vec![LoaderReq {
                loader: "fabric".into(),
                range: ">=0.16.9".into(),
            }]
        );
        assert!(
            m.deps.iter().all(|d| d.modid != "fabricloader"),
            "still not a mod edge"
        );
        // the Fabric API is a real mod and stays one
        assert!(m.deps.iter().any(|d| d.modid == "fabric-api"));
    }

    // More Enchantment Info's NeoForge build loads through KotlinLangForge, and
    // says so only in `modLoader`. The two bundled language loaders name no
    // dependency.
    #[test]
    fn a_language_loader_another_mod_provides_is_read() {
        let klf = parse_mods_toml("modLoader = \"klf\"\n[[mods]]\nmodId=\"more_enchantment_info\"");
        assert_eq!(klf.language_loader.as_deref(), Some("klf"));
        for bundled in ["javafml", "lowcodefml"] {
            let m = parse_mods_toml(&format!("modLoader=\"{bundled}\"\n[[mods]]\nmodId=\"m\""));
            assert_eq!(m.language_loader, None, "{bundled}");
        }
        assert_eq!(
            parse_mods_toml("[[mods]]\nmodId=\"m\"").language_loader,
            None
        );
    }

    #[test]
    fn legacy_mandatory_flag_maps_to_required_or_optional() {
        let toml = r#"
            [[mods]]
            modId="oldmod"
            [[dependencies.oldmod]]
            modId="hardlib"
            mandatory=true
            [[dependencies.oldmod]]
            modId="softlib"
            mandatory=false
        "#;
        let m = parse_mods_toml(toml);
        let kind = |id: &str| m.deps.iter().find(|d| d.modid == id).map(|d| d.kind);
        assert_eq!(kind("hardlib"), Some(RelKind::Requires));
        assert_eq!(kind("softlib"), Some(RelKind::OptionalDep));
    }

    #[test]
    fn parses_fabric_mod_json_dep_buckets() {
        let json = br#"{
            "id": "mymod",
            "depends": {"fabric-api": "*", "cloth-config": ">=11.0", "minecraft": ">=1.20"},
            "recommends": {"modmenu": "*"},
            "breaks": {"oldmod": "*"},
            "conflicts": {"grumpymod": "*"}
        }"#;
        let m = parse_fabric_json(json);
        assert_eq!(m.modid.as_deref(), Some("mymod"));
        let find = |id: &str| m.deps.iter().find(|d| d.modid == id);
        // required, wildcard range dropped
        assert_eq!(find("fabric-api").map(|d| d.kind), Some(RelKind::Requires));
        assert_eq!(find("fabric-api").unwrap().version_range, None);
        // required with a real range kept
        assert_eq!(
            find("cloth-config").unwrap().version_range.as_deref(),
            Some(">=11.0")
        );
        assert_eq!(find("modmenu").map(|d| d.kind), Some(RelKind::Recommends));
        // Fabric's `breaks` is the hard one and its `conflicts` the advisory one
        // -- one kind, two intensities, and the names do not decide which (#129)
        let sev = |m: &str| find(m).and_then(|d| d.severity);
        assert_eq!(find("oldmod").map(|d| d.kind), Some(RelKind::Conflicts));
        assert_eq!(sev("oldmod"), Some(Severity::Hard));
        assert_eq!(find("grumpymod").map(|d| d.kind), Some(RelKind::Conflicts));
        assert_eq!(sev("grumpymod"), Some(Severity::Soft));
        // minecraft filtered
        assert!(find("minecraft").is_none());
    }

    #[test]
    fn extracts_display_name_version_and_logo() {
        // per-mod logoFile wins over the top-level one; placeholder version
        // passes through verbatim (resolved later against the jar manifest)
        let toml = r#"
            logoFile="assets/top.png"
            [[mods]]
            modId="configured"
            displayName="Configured"
            version="${file.jarVersion}"
            logoFile="/configured.png"
        "#;
        let m = parse_mods_toml(toml);
        assert_eq!(m.display_name.as_deref(), Some("Configured"));
        assert_eq!(m.version.as_deref(), Some("${file.jarVersion}"));
        assert_eq!(m.logo_file.as_deref(), Some("configured.png"));

        // top-level logo serves when the mod entry has none
        let top = parse_mods_toml("logoFile=\"logo.png\"\n[[mods]]\nmodId=\"m\"");
        assert_eq!(top.logo_file.as_deref(), Some("logo.png"));

        // fabric: name/version/icon (map form picks any entry)
        let f = parse_fabric_json(
            br#"{"id":"sodium","name":"Sodium","version":"0.6.13","icon":{"32":"/assets/sodium/icon.png"}}"#,
        );
        assert_eq!(f.display_name.as_deref(), Some("Sodium"));
        assert_eq!(f.version.as_deref(), Some("0.6.13"));
        assert_eq!(f.logo_file.as_deref(), Some("assets/sodium/icon.png"));
    }

    #[test]
    fn read_mod_meta_prefers_toml_and_tolerates_junk() {
        // a jar carrying a neoforge.mods.toml
        let jar = super::super::classfile::fixtures::jar(&[(
            "META-INF/neoforge.mods.toml",
            br#"[[mods]]
modId="fromtoml"
[[dependencies.fromtoml]]
modId="lib"
type="required""#,
        )]);
        let m = read_mod_meta(&jar);
        assert_eq!(m.modid.as_deref(), Some("fromtoml"));
        assert_eq!(m.deps.len(), 1);
        assert_eq!(m.deps[0].modid, "lib");

        // non-jar bytes -> empty, no panic
        assert!(read_mod_meta(b"not a zip").modid.is_none());
    }
}
