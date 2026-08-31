//! Registry harvest: scan the cache + published manifests, read each jar in a
//! single pass (bytecode graph + side, declared metadata, mcmod.info),
//! batch-resolve Modrinth identity, and reconcile it all into the registry. The
//! scan (FS + network) is async, with the per-jar parsing offloaded to a blocking
//! task; the write (`write_scan`) is a pure, transactional DB step unit-tested
//! without I/O.
//!
//! Harvest-only: it writes `source = harvested | jar-meta | inferred | modrinth`
//! rows (plus `curator` from published conflicts) and never clobbers authored
//! ones. Dependency facts come by trust: Modrinth `version.dependencies`
//! (authoritative for a Modrinth mod that declares any -- targets in the
//! `modrinth:<project_id>` selector namespace), else the jar's own declaration
//! (mcmod.info, or modern mods.toml / neoforge.mods.toml / fabric.mod.json) plus
//! bytecode inference. The derived layers (inferred + modrinth) are rebuilt each
//! run; consuming them into a resolver is separate (#42).

use super::archive::read_zip_entry;
use super::bytecode;
use super::classfile::parse_class;
use super::curator::{
    JarFacts, McModInfo, clean_mc_version, mcmod_hard_deps, mcmod_modids, parse_mcmod_info,
};
use super::mixinscan;
use super::modmeta;
use super::modrinth::{Modrinth, Project};
use crate::registry::model::{RelKind, Severity, Source};
use crate::registry::{Registry, queries, upsert};
use crate::storage::Storage;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A cached/manifested jar reduced to the facts the registry needs.
#[derive(Clone)]
pub struct JarSeed {
    pub sha1: String,
    pub size_bytes: i64,
    pub modid: Option<String>,
    // Every modid the jar's mcmod.info declares when it bundles more than one mod
    // (ForgeMultipart, ProjectRed, ReplayMod). Each becomes a modid alias on the
    // jar's mod, so a dependency on a bundled modid resolves to the jar shipping it.
    // Empty for a single-mod jar (its one modid is already `modid`).
    pub extra_modids: Vec<String>,
    pub version: Option<String>,
    pub project_id: Option<String>,
    pub loaders: Vec<String>,
    pub mc_versions: Vec<String>,
    pub requires: Vec<String>, // jar-meta dep modids (pseudo-deps filtered out)
    pub filename: Option<String>,
    // human metadata for the panel's mod browser: display name (jar-meta name ->
    // Modrinth title), Modrinth slug, author (jar-meta authorList -> team owner).
    pub name: Option<String>,
    pub author: Option<String>,
    pub slug: Option<String>,
    // Modrinth version id (from the sha1 lookup), so a build's Modrinth-sourced
    // mod can be re-added as a Modrinth source, not a non-existent cache jar.
    pub modrinth_version_id: Option<String>,
    // Release channel (release/beta/dev/unknown): from Modrinth version_type when
    // known, else unknown -- a jar carries no reliable channel of its own.
    pub channel: Option<String>,
    // Bytecode-derived facts (empty for a jar with no local bytes, e.g. a
    // Modrinth-only mod). owned_packages seed the package->owner index; the ref
    // sets become inferred requires/optional_dep edges once that index is built.
    pub owned_packages: Vec<String>,
    /// Binary names of every class the jar carries, for the digest that answers
    /// "does this artifact still have class C?" (#145). Empty for a jar with no
    /// local bytes, which reads as "cannot answer".
    pub class_names: Vec<String>,
    /// `(config, needed binary name)` for every `required` mixin config (#145).
    pub mixin_needs: Vec<(String, String)>,
    pub hard_refs: Vec<String>,
    pub optional_refs: Vec<String>,
    // Derived classification (stage D): what the jar is and how it must match
    // the server. `kind = None` means the jar was never scanned (no local
    // bytes), which is different from a scanned jar the classifier could not
    // decide (kind present, side/policy None).
    pub side: Option<String>,
    pub side_confidence: Option<String>,
    pub match_policy: Option<String>,
    pub kind: Option<String>,
    // Modrinth `version.dependencies` for a Modrinth-identified jar. This is
    // Modrinth's curated dependency graph -- more reliable than either a jar
    // declaration or bytecode -- so it is authoritative and suppresses every
    // other dependency source for the same mod. Includes external dependencies
    // (no target project, a bare file name): the hybrid case.
    pub modrinth_deps: Vec<ModrinthDepSeed>,
    // Modern declared deps (mods.toml / neoforge.mods.toml / fabric.mod.json):
    // typed, version-ranged. Emitted for a non-Modrinth jar; the target modid,
    // its relation kind, and an optional version range.
    pub declared_deps: Vec<(String, RelKind, Option<Severity>, Option<String>)>,
    /// `(loader, range)` windows the jar declares on the loader build itself
    /// (#164). `None` when this pass never opened the jar -- a Modrinth-only
    /// mod, whose window the loader-window check reads by range instead --
    /// which is not the same as a jar that opened and declared none.
    pub loader_reqs: Option<Vec<(String, String)>>,
    // JVM runtime languages (scala/kotlin) the jar needs on the classpath but a
    // modern-fork loader does not bundle, and those it provides itself. Emitted
    // as `runtime:<lang>` capability requires/provides so the resolver flags a
    // pack whose scala/kotlin mods have no provider (Scalar). See bytecode.rs.
    pub needs_runtime: Vec<String>,
    pub provides_runtime: Vec<String>,
}

/// One Modrinth version dependency as the seed carries it: the target project
/// (`None` for an external dependency), its type, a pinned version id, and the
/// external file name when the target lives outside Modrinth.
#[derive(Clone, Default)]
pub struct ModrinthDepSeed {
    pub project_id: Option<String>,
    pub dep_type: String,
    pub version_id: Option<String>,
    pub file_name: Option<String>,
}

pub struct BuildModSeed {
    pub sha1: String,
    pub filename: String,
    pub required: bool,
    pub default_enabled: bool,
}

pub struct PackSeed {
    pub pack_id: String,
    pub pack_version: String,
    pub mc_version: String,
    pub loader_id: Option<String>,
    pub loader_version: Option<String>,
    pub java_major: Option<i64>,
    pub fingerprint: Option<String>,
    pub mods: Vec<BuildModSeed>,
    pub conflicts: Vec<(String, String)>, // (a_sha1, b_sha1), from display.incompatible_with
}

#[derive(Default)]
pub struct ScanData {
    pub jars: Vec<JarSeed>,
    pub packs: Vec<PackSeed>,
    /// Forge modids learned this scan by fetching a Modrinth re-upload's jar (a
    /// mod present only via Modrinth, whose bytes are not in the local cache).
    pub modrinth_modids_learned: usize,
    /// `project_id -> slug` for every Modrinth project named as a dependency
    /// target, so `write_scan` can link a `modrinth:<project>` dependency to a
    /// self-hosted provider whose forge modid matches the project slug.
    pub dep_project_slugs: HashMap<String, String>,
    /// `project_id -> (client_side, server_side)` for every project object this
    /// scan fetched. Applied by `write_scan` through the `modrinth` alias once
    /// aliases are settled, so the flags land on whatever mod owns the project
    /// -- including a self-hosted provider linked to it by slug.
    pub project_envs: HashMap<String, (String, String)>,
    /// Whether the Modrinth sha1-identity leg of this scan actually answered.
    /// False on an error AND on a suspiciously empty answer (hashes sent,
    /// nothing matched -- the shape a degraded upstream returns): `write_scan`
    /// then keeps the last good `modrinth` relations instead of wiping the
    /// layer and rewriting it with nothing.
    pub modrinth_leg_ok: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HarvestReport {
    pub jars_scanned: usize,
    pub jars_no_identity: usize,
    pub mods: i64,
    pub mod_versions: i64,
    pub relations: i64,
    pub packs: usize,
    pub builds: i64,
    /// Bytecode-derived hard-dependency edges written this harvest.
    pub inferred_requires: i64,
    /// Bytecode-derived optional-dependency edges written this harvest.
    pub inferred_optional: i64,
    /// Jars whose client/server side was derived from the bytecode.
    pub sides_derived: i64,
    /// Jars whose server-match policy was derived from the bytecode.
    pub match_policies_derived: i64,
    /// Scanned jars classified as not-a-mod (coremod / bare library).
    pub non_mod_jars: i64,
    /// Dependency edges taken from Modrinth `version.dependencies` this harvest.
    pub modrinth_deps: i64,
    /// Typed dependency edges taken from modern declared metadata (mods.toml /
    /// neoforge.mods.toml / fabric.mod.json) this harvest.
    pub declared_deps: i64,
    /// Forge modids learned this harvest by reading a Modrinth re-upload's jar, so
    /// a modid-keyed dependency (an addon requiring `ic2`) resolves to the mod the
    /// pack ships from Modrinth. Fetched once per mod, then cached as an alias.
    pub modrinth_modids_learned: usize,
    /// Self-hosted mods linked to a Modrinth project this harvest, so a
    /// `modrinth:<project>` dependency resolves to a provider the mirror re-hosts
    /// under its forge modid (a project-keyed dep pointing at a self-hosted jar).
    pub modrinth_selfhost_links: i64,
    /// Identity splits folded this harvest by matching a Modrinth mod's slug to
    /// another mod's forge modid (the two-jar re-upload case no artifact bridges).
    pub identities_reconciled: i64,
    /// `loader:<name>` capabilities emitted this harvest for known bridge mods,
    /// so a foreign-loader artifact a connector carries reads as carried rather
    /// than as dead weight.
    pub loader_bridges: i64,
}

/// Confidence rank for modern-manifest declared deps: above the shared
/// `jar-meta` rank (mcmod.info), below `authored`, so per-target dedup prefers
/// the loader-enforced declaration when a dual-metadata jar carries both.
const MANIFEST_DEP_RANK: i64 = 55;

/// Mods that expose an *optional* integration API -- item viewers and probe/
/// tooltip mods. A jar references one of these from a plugin class (`@JeiPlugin`
/// and the like) that the host loads only when present, so a *bytecode-inferred*
/// reference to it is a dormant integration, not a hard dependency (FTB Library
/// references JEI from its plugin but runs fine on REI). A mod that truly requires
/// one declares it in its metadata, which yields a declared edge that stays hard --
/// this downgrade only touches the inferred kind.
const INTEGRATION_HOSTS: &[&str] = &[
    "jei",
    "roughlyenoughitems",
    "emi",
    "jade",
    "waila",
    "hwyla",
    "wthit",
    "theoneprobe",
    // sorting host: containers advertise themselves to it through API
    // annotations and helper signatures, which read as unconditional
    // references at class granularity (Forestry, IronChest)
    "inventorytweaks",
];

/// Whether an edge selector names an [`INTEGRATION_HOSTS`] mod, ignoring a version
/// window (`jei@[...]`) and the `modrinth:` namespace.
fn is_integration_host(target: &str) -> bool {
    let bare = target.split('@').next().unwrap_or(target);
    let bare = bare.strip_prefix("modrinth:").unwrap_or(bare);
    INTEGRATION_HOSTS.contains(&bare)
}

/// Map a Modrinth `version_type` (release/beta/alpha) to a registry channel.
/// alpha collapses to beta (both pre-release); `dev` stays reserved for hand-set
/// developer builds. Unknown types yield None (release stays `unknown`).
/// `Implementation-Version` from a jar MANIFEST.MF -- the value gradle's
/// `${file.jarVersion}` placeholder stands for. Header names are
/// case-insensitive per the jar spec; continuation lines are not handled (a
/// version string never needs one).
fn manifest_implementation_version(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':')
            && key.trim().eq_ignore_ascii_case("Implementation-Version")
        {
            let v = value.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn channel_from_version_type(vt: &str) -> Option<String> {
    // Modrinth's own vocabulary, kept verbatim -- the same channel dialect
    // pack builds store, so the whole mirror speaks one set of values.
    match vt {
        "release" | "beta" | "alpha" => Some(vt.to_string()),
        _ => None,
    }
}

fn ae(e: crate::http::ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Resolve a referenced package prefix to a selector for its owning mod, for an
/// inferred edge from `from_mod_id`. Prefers the owner's modid; falls back to its
/// `modrinth:<project_id>` selector when it has no modid (a Modrinth-only owner),
/// so a package-indexed target is not silently dropped. `None` when the prefix
/// has no single owner, its owner is the referencing mod itself, or the owner has
/// neither identity.
fn resolve_edge_target(
    conn: &Connection,
    prefix: &str,
    from_mod_id: i64,
) -> Result<Option<String>> {
    let Some(owner) = queries::owner_mod_for_prefix(conn, prefix)? else {
        return Ok(None);
    };
    if owner == from_mod_id {
        return Ok(None);
    }
    if let Some(modid) = queries::modid_for_mod(conn, owner)? {
        return Ok(Some(modid));
    }
    Ok(queries::modrinth_id_for_mod(conn, owner)?.map(|pid| format!("modrinth:{pid}")))
}

/// Resolve a Modrinth EXTERNAL dependency's file name to a registry selector:
/// an artifact the mirror knows by that exact filename (its mod's modid, else
/// its `modrinth:` alias), else the filename stem as a case-insensitive modid
/// (`CustomNPCs-1.12.jar` -> `customnpcs`). `None` -> genuinely external.
fn external_dep_selector(conn: &Connection, file_name: &str) -> Result<Option<String>> {
    if let Some(owner) = queries::mod_id_for_filename(conn, file_name)? {
        if let Some(modid) = queries::modid_for_mod(conn, owner)? {
            return Ok(Some(modid));
        }
        if let Some(pid) = queries::modrinth_id_for_mod(conn, owner)? {
            return Ok(Some(format!("modrinth:{pid}")));
        }
    }
    let stem = file_name.trim_end_matches(".jar");
    let stem = stem.split(['-', '_', ' ']).next().unwrap_or(stem);
    if !stem.is_empty() && queries::mod_id_for_selector(conn, stem)?.is_some() {
        return Ok(Some(stem.to_string()));
    }
    Ok(None)
}

/// Map a Modrinth `dependency_type` to a relation kind. `embedded` (a bundled
/// jar-in-jar library) is not an external requirement and yields no edge; an
/// unknown type is ignored.
fn modrinth_rel_kind(dep_type: &str) -> Option<(RelKind, Option<Severity>)> {
    match dep_type {
        "required" => Some((RelKind::Requires, None)),
        "optional" => Some((RelKind::OptionalDep, None)),
        // Modrinth has one incompatibility and means the hard one by it
        "incompatible" => Some((RelKind::Conflicts, Some(Severity::Hard))),
        _ => None,
    }
}

/// What one jar contributes to the mixin check (#145): the classes it carries,
/// and `(config, needed)` for every class its required mixins must resolve.
type MixinReadout = (Vec<String>, Vec<(String, String)>);

/// Everything the harvest reads from one jar. Public so the corpus runner can
/// classify real jars through the exact production path.
pub struct JarReadout {
    pub facts: JarFacts,
    pub bytecode: bytecode::JarBytecode,
    pub modmeta: modmeta::ModMeta,
    pub mcmod: Option<McModInfo>,
    /// Every modid the jar's mcmod.info declares -- more than one when the jar
    /// bundles several mods. Empty when the jar has no mcmod.info.
    pub mcmod_modids: Vec<String>,
    /// What this jar's `required` mixin configs must be able to resolve (#145).
    /// Empty for a jar with no required config, which is most of them.
    pub required_mixins: Vec<mixinscan::RequiredMixins>,
    /// Binary names of every class the jar carries, for the digest that answers
    /// whether an artifact still has a given class (#145).
    pub class_names: Vec<String>,
}

/// Open a jar's zip ONCE and derive every fact the harvest needs from it: the
/// loader marker, the bytecode graph + classification, the modern declared
/// metadata, mcmod.info, and the manifest/mixin coremod markers. Replaces
/// several separate zip opens per jar. Best-effort -- a non-zip or truncated
/// jar yields empty facts.
pub fn read_jar(bytes: &[u8]) -> JarReadout {
    let empty = || JarReadout {
        facts: JarFacts::default(),
        bytecode: bytecode::JarBytecode::default(),
        modmeta: modmeta::ModMeta::default(),
        mcmod: None,
        mcmod_modids: Vec::new(),
        required_mixins: Vec::new(),
        class_names: Vec::new(),
    };
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return empty();
    };

    let mut classes = Vec::new();
    let mut mcmod_raw: Option<Vec<u8>> = None;
    let mut mods_toml: Option<Vec<u8>> = None; // neoforge.mods.toml wins over mods.toml
    let mut fabric_json: Option<Vec<u8>> = None;
    let mut has_forge = false;
    let mut has_neoforge = false;
    let mut has_fabric = false;
    let mut impl_version: Option<String> = None;
    let mut signals = bytecode::JarSignals::default();
    let mut mixin_configs: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let size = entry.size();
        if name.ends_with(".class") {
            if let Ok(b) = read_zip_entry(&mut entry, size, &name)
                && let Some(info) = parse_class(&b)
            {
                classes.push(info);
            }
            continue;
        }
        if bytecode::is_mixin_config_name(&name) {
            signals.mixin_configs += 1;
            if let Ok(raw) = read_zip_entry(&mut entry, size, &name) {
                mixin_configs.insert(name.clone(), raw);
            }
            continue;
        }
        match name.as_str() {
            "mcmod.info" => {
                has_forge = true;
                mcmod_raw = read_zip_entry(&mut entry, size, &name).ok();
            }
            "META-INF/mods.toml" => {
                has_forge = true;
                if mods_toml.is_none() {
                    mods_toml = read_zip_entry(&mut entry, size, &name).ok();
                }
            }
            "META-INF/neoforge.mods.toml" => {
                has_neoforge = true;
                mods_toml = read_zip_entry(&mut entry, size, &name).ok();
            }
            "fabric.mod.json" => {
                has_fabric = true;
                fabric_json = read_zip_entry(&mut entry, size, &name).ok();
            }
            "META-INF/MANIFEST.MF" => {
                if let Ok(raw) = read_zip_entry(&mut entry, size, &name) {
                    let (coremod, tweaker) = bytecode::manifest_markers(&raw);
                    signals.manifest_coremod = coremod;
                    signals.manifest_tweaker = tweaker;
                    impl_version = manifest_implementation_version(&raw);
                }
            }
            _ => {}
        }
    }

    let mut modmeta = if let Some(t) = mods_toml.as_deref() {
        std::str::from_utf8(t)
            .map(modmeta::parse_mods_toml)
            .unwrap_or_default()
    } else if let Some(f) = fabric_json.as_deref() {
        modmeta::parse_fabric_json(f)
    } else {
        modmeta::ModMeta::default()
    };
    // A gradle placeholder version (`${file.jarVersion}`) resolves to the jar
    // manifest's Implementation-Version; an unresolvable placeholder is no
    // version at all, never a literal `${...}` string in the registry.
    if modmeta.version.as_deref().is_some_and(|v| v.contains("${")) {
        modmeta.version = impl_version.clone();
    }
    let mcmod = mcmod_raw
        .as_deref()
        .and_then(parse_mcmod_info)
        .filter(|i| !i.modid.is_empty());
    bytecode::apply_fabric_meta(&mut signals, &modmeta);
    signals.meta_identity = mcmod.is_some() || modmeta.modid.is_some();
    signals.display_test_tolerant = modmeta.display_test_tolerant();
    let bytecode = bytecode::aggregate(&classes, &signals);
    // `@Mod` is a Forge-specific annotation, so a jar carrying one is Forge even
    // when it ships no mcmod.info / mods.toml marker file (older mods often do not).
    // The NeoForge marker file is the one loader signal that separates a NeoForge
    // build from a Forge one, so it wins over everything.
    let loader = if has_neoforge {
        Some("neoforge".to_string())
    } else if has_forge || bytecode.mod_id.is_some() {
        Some("forge".to_string())
    } else if has_fabric {
        Some("fabric".to_string())
    } else {
        None
    };
    let class_names: Vec<String> = classes.iter().map(|c| c.this_class.clone()).collect();
    let required_mixins = mixinscan::from_parts(
        mods_toml.as_deref(),
        fabric_json.as_deref(),
        &mixin_configs,
        &classes,
    );
    JarReadout {
        facts: JarFacts { loader },
        bytecode,
        modmeta,
        mcmod,
        mcmod_modids: mcmod_raw.as_deref().map(mcmod_modids).unwrap_or_default(),
        required_mixins,
        class_names,
    }
}

/// Reconcile a scan into the registry, in one transaction. Pure (no I/O beyond
/// the connection); idempotent; never clobbers authored rows.
pub fn write_scan(conn: &Connection, scan: &ScanData, now: &str) -> Result<HarvestReport> {
    // sha1 -> modid, so curator conflict targets (selectors) can be expressed
    let modid_by_sha: HashMap<&str, &str> = scan
        .jars
        .iter()
        .filter_map(|j| j.modid.as_deref().map(|m| (j.sha1.as_str(), m)))
        .collect();

    // The derived layers are purely rebuildable: wipe the package index and the
    // inferred + Modrinth edges up front, then re-derive from this scan. jar-meta
    // edges are cleared per artifact as each is re-derived below (so a jar no
    // longer cached keeps its edges); authored/curator relations are a different
    // source and untouched.
    conn.execute("DELETE FROM mod_package", [])?;
    if scan.modrinth_leg_ok {
        conn.execute(
            "DELETE FROM relation WHERE source IN ('inferred', 'modrinth')",
            [],
        )?;
    } else {
        // the Modrinth identity leg did not answer this run: rewrite only the
        // locally-derived layer and keep the last good modrinth relations --
        // upstream weather must not erase derived state
        tracing::warn!("modrinth leg degraded; keeping last good modrinth relations");
        conn.execute("DELETE FROM relation WHERE source = 'inferred'", [])?;
    }

    let mut sides_derived = 0i64;
    let mut match_policies_derived = 0i64;
    let mut non_mod_jars = 0i64;
    let mut modrinth_deps_written = 0i64;
    let mut bridges_written = 0i64;
    let mut declared_deps_written = 0i64;
    // (from_mod_id, jar) for jars carrying references, resolved to edges in a
    // second pass once every jar's packages are in the index.
    let mut derivations: Vec<(i64, i64, &JarSeed)> = Vec::new();

    // external Modrinth deps deferred past jar registration (the hybrid case)
    let mut external_deps: Vec<(i64, i64, RelKind, Option<Severity>, String)> = Vec::new();
    let mut no_identity = 0usize;
    for jar in &scan.jars {
        // Per-jar classification, keyed by content hash -- recorded for every
        // scanned jar BEFORE the identity gate: a bare coremod/library jar has
        // no mod row, yet the resolve report still needs its kind.
        // What was read out of it, identified or not (#123). The unidentified
        // ones are exactly the population whose names are otherwise lost, and
        // this file is already open.
        upsert::set_jar_read(
            conn,
            &jar.sha1,
            &upsert::JarRead {
                modid: jar.modid.as_deref(),
                name: jar.name.as_deref(),
                version: jar.version.as_deref(),
                loaders: &jar.loaders,
                mc: &jar.mc_versions,
                filename: jar.filename.as_deref(),
            },
        )?;
        if let Some(kind) = jar.kind.as_deref() {
            upsert::set_jar_class(
                conn,
                &jar.sha1,
                kind,
                jar.side.as_deref(),
                jar.match_policy.as_deref(),
                jar.side_confidence.as_deref(),
            )?;
            if jar.side.is_some() {
                sides_derived += 1;
            }
            if jar.match_policy.is_some() {
                match_policies_derived += 1;
            }
            if kind != "mod" {
                non_mod_jars += 1;
            }
        }
        let mut aliases: Vec<(&str, &str)> = Vec::new();
        if let Some(m) = jar.modid.as_deref().filter(|s| !s.is_empty()) {
            aliases.push(("modid", m));
        }
        if let Some(p) = jar.project_id.as_deref().filter(|s| !s.is_empty()) {
            aliases.push(("modrinth", p));
        }
        // a jar bundling several mods declares each as a modid alias, so a
        // dependency on any bundled modid resolves to the jar that ships it
        for m in &jar.extra_modids {
            if !m.is_empty() {
                aliases.push(("modid", m.as_str()));
            }
        }
        if aliases.is_empty() {
            no_identity += 1;
            continue;
        }
        let mod_id = upsert::upsert_mod_by_alias(conn, &aliases, now)?;
        upsert::set_mod_meta(
            conn,
            mod_id,
            jar.name.as_deref(),
            jar.slug.as_deref(),
            jar.author.as_deref(),
            now,
        )?;
        // every loader the artifact suits; empty -> 'any' (handled downstream)
        let targets: Vec<&str> = jar.loaders.iter().map(String::as_str).collect();
        let version = jar.version.as_deref().unwrap_or("unknown");
        let mc_versions = if jar.mc_versions.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&jar.mc_versions)?)
        };
        // the artifact this jar is: every edge derived below belongs to it, not to
        // the mod, so a second version of the same mod cannot lend it its deps (#48)
        let mod_version_id = upsert::upsert_mod_version(
            conn,
            mod_id,
            version,
            &targets,
            &jar.sha1,
            jar.size_bytes,
            jar.filename.as_deref(),
            mc_versions.as_deref(),
            now,
        )?;
        // after the row exists: record its Modrinth version id (by sha1)
        upsert::set_mod_version_modrinth(conn, &jar.sha1, jar.modrinth_version_id.as_deref(), now)?;
        // group the file under the release for its (version, channel)
        let channel = jar.channel.as_deref().unwrap_or("unknown");
        upsert::set_harvested_release(conn, &jar.sha1, mod_id, version, channel, now)?;
        // A Modrinth mod's deps come from version.dependencies (below), which is
        // authoritative -- but only when Modrinth actually declares some. An empty
        // upstream list is more likely unfilled than a genuine "no dependencies",
        // so it does NOT suppress the jar's own declaration or the bytecode: we
        // never want a Modrinth mod to end up with zero edges just because its
        // Modrinth page is bare.
        let is_modrinth = jar.project_id.is_some() && !jar.modrinth_deps.is_empty();
        // A modern loader manifest is loader-enforced: a genuinely hard dep MUST
        // be declared there, so for such a jar the manifest is the authoritative
        // dependency source and bytecode inference adds only noise (the per-mod
        // best-source cascade). 1.12-era mcmod.info stays the legacy tier where
        // the bytecode fallback is still needed.
        let has_manifest_deps = !jar.declared_deps.is_empty();

        // Re-derive this artifact's jar-meta edges from scratch: drop the ones a
        // previous harvest wrote -- possibly under an older, buggier dependency
        // parse -- before writing the current ones, so a stale malformed target
        // (a comma-joined list, a human-readable phrase) does not linger beside the
        // clean one (#10). Scoped to this artifact, so jar-meta edges of jars no
        // longer in the cache are left alone.
        conn.execute(
            "DELETE FROM relation WHERE from_mod_version_id = ?1 AND source = 'jar-meta'",
            [mod_version_id],
        )?;

        // What this artifact provides and what its required mixins demand (#145).
        // Only written when the jar's bytes were actually read: an empty list
        // from a Modrinth-only mod would otherwise be stored as "provides
        // nothing", which is a lie the check would act on.
        if !jar.class_names.is_empty() {
            upsert::set_class_digest(conn, mod_version_id, &jar.class_names)?;
        }
        if !jar.class_names.is_empty() {
            upsert::set_mixin_needs(conn, mod_version_id, &jar.mixin_needs)?;
        }

        // What the jar demands of the loader build (#164). Written only for a
        // jar this pass actually opened: recording "declares nothing" for one
        // it never read would answer the check with a jar it has not seen.
        if let Some(reqs) = &jar.loader_reqs {
            upsert::set_artifact_loader_reqs(conn, &jar.sha1, reqs, now)?;
        }

        // Declared deps (author-written) go in for a non-authoritative-Modrinth
        // jar: mcmod.info modids for 1.12.2, plus typed + version-ranged deps from
        // modern metadata.
        if !is_modrinth {
            // Manifest deps first: the relation dedupe key ignores confidence,
            // so when a dual-metadata jar declares the same target in both its
            // manifest and its mcmod.info, the first write occupies the slot --
            // and the loader-enforced manifest is the one that should.
            for (target, kind, severity, range) in &jar.declared_deps {
                if upsert::upsert_relation_ranked(
                    conn,
                    mod_id,
                    Some(mod_version_id),
                    target,
                    range.as_deref(),
                    *kind,
                    *severity,
                    Source::JarMeta,
                    MANIFEST_DEP_RANK,
                    now,
                )? {
                    declared_deps_written += 1;
                }
            }
            for dep in &jar.requires {
                upsert::upsert_relation(
                    conn,
                    mod_id,
                    Some(mod_version_id),
                    dep,
                    None,
                    RelKind::Requires,
                    None,
                    Source::JarMeta,
                    now,
                )?;
            }
        }

        // bytecode-derived facts: this jar's owned packages into the index
        // (the classification went to jar_class above); its references are
        // resolved to edges after the loop.
        let owned: Vec<&str> = jar.owned_packages.iter().map(String::as_str).collect();
        upsert::set_mod_packages(conn, mod_id, &owned)?;

        // Emit Modrinth's curated deps whenever the jar is Modrinth-identified.
        // A project-keyed dep targets the `modrinth:<project_id>` selector
        // namespace, with the exact pinned version_id (when pinned) in the
        // version slot rather than a Maven range. An EXTERNAL dep (no target
        // project, a bare file name -- the hybrid case) resolves through the
        // mirror's own knowledge: an artifact by that exact filename, else a
        // modid guessed from the stem; unresolved it lands in the
        // `external:<file_name>` namespace so the resolver reports it as an
        // out-of-ecosystem dependency instead of a resolver bug.
        // A bridge carries another loader's artifacts at runtime; the resolver
        // reads that as the `loader:<name>` capability, and the shipped bridge
        // table is where the fact comes from. Emitted per artifact, like every
        // other harvested edge, so it dies with the jar rather than lingering.
        if let Some(pid) = jar.project_id.as_deref()
            && let Some(carried) = queries::bridged_loader_for_project(conn, pid)?
            && upsert::upsert_relation(
                conn,
                mod_id,
                Some(mod_version_id),
                &format!("loader:{carried}"),
                None,
                RelKind::Provides,
                None,
                Source::Harvested,
                now,
            )?
        {
            bridges_written += 1;
        }

        // Classpath runtime languages (scala/kotlin). Not a mod and never a
        // package edge (STOP_PREFIXES), but a modern-fork loader ships no
        // Scala/Kotlin, so a jar referencing one crashes at classload unless a
        // provider is in the pack. Emitted as a `runtime:<lang>` capability --
        // provides from the runtime carrier (Scalar), requires from every mod
        // that needs it -- so the resolver's existing capability-satisfaction
        // pass reports the gap. Ungated: it applies to cache and Modrinth jars
        // alike, and Modrinth's curated deps never model a JVM-runtime need.
        for rt in &jar.provides_runtime {
            upsert::upsert_relation(
                conn,
                mod_id,
                Some(mod_version_id),
                &format!("runtime:{rt}"),
                None,
                RelKind::Provides,
                None,
                Source::Inferred,
                now,
            )?;
        }
        for rt in &jar.needs_runtime {
            upsert::upsert_relation(
                conn,
                mod_id,
                Some(mod_version_id),
                &format!("runtime:{rt}"),
                None,
                RelKind::Requires,
                None,
                Source::Inferred,
                now,
            )?;
        }

        if let Some(pid) = jar.project_id.as_deref() {
            for dep in &jar.modrinth_deps {
                let Some((kind, severity)) = modrinth_rel_kind(&dep.dep_type) else {
                    continue;
                };
                let (target, range) = match (&dep.project_id, &dep.file_name) {
                    (Some(dep_pid), _) => {
                        if dep_pid == pid {
                            continue; // a project never depends on itself
                        }
                        (format!("modrinth:{dep_pid}"), dep.version_id.as_deref())
                    }
                    (None, Some(fname)) => {
                        // resolved after the loop: the filename bridge needs
                        // every jar registered first
                        external_deps.push((mod_id, mod_version_id, kind, severity, fname.clone()));
                        continue;
                    }
                    (None, None) => continue, // nothing to key the target on
                };
                if upsert::upsert_relation(
                    conn,
                    mod_id,
                    Some(mod_version_id),
                    &target,
                    range,
                    kind,
                    severity,
                    Source::Modrinth,
                    now,
                )? {
                    modrinth_deps_written += 1;
                }
            }
        }

        if !is_modrinth
            && !has_manifest_deps
            && (!jar.hard_refs.is_empty() || !jar.optional_refs.is_empty())
        {
            derivations.push((mod_id, mod_version_id, jar));
        }
    }

    // Second pass: resolve each referenced package prefix to its owning mod and
    // record an inferred edge. A prefix with no single owner (unknown or shaded)
    // is skipped; a hard edge to a target suppresses a competing optional one.
    let mut inferred_requires = 0i64;
    let mut inferred_optional = 0i64;
    // Hard pass first, keyed by artifact. This used to accumulate across all of a
    // mod's jars so that a hard reference in one artifact suppressed a soft one in
    // another -- a workaround for edges being mod-level, where two artifacts'
    // contradictory rows landed on the same node. Now that an edge names the jar it
    // came from (#48) each artifact simply states its own facts: a jar's own hard
    // reference suppresses its own soft one, and nothing leaks between builds.
    let mut hard_by_artifact: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut opt_by_artifact: HashMap<i64, HashSet<String>> = HashMap::new();
    for (from_mod_id, mod_version_id, jar) in &derivations {
        for prefix in &jar.hard_refs {
            let Some(target) = resolve_edge_target(conn, prefix, *from_mod_id)? else {
                continue;
            };
            // A bytecode reference to an optional-integration host (item viewer,
            // probe) is a dormant plugin hook, not a hard dependency -- record it
            // optional so it never reports missing. A genuine requirement is declared
            // in the mod's metadata, whose higher-confidence edge wins over this one.
            if is_integration_host(&target) {
                if opt_by_artifact
                    .entry(*mod_version_id)
                    .or_default()
                    .insert(target.clone())
                    && upsert::upsert_relation(
                        conn,
                        *from_mod_id,
                        Some(*mod_version_id),
                        &target,
                        None,
                        RelKind::OptionalDep,
                        None,
                        Source::Inferred,
                        now,
                    )?
                {
                    inferred_optional += 1;
                }
            } else if hard_by_artifact
                .entry(*mod_version_id)
                .or_default()
                .insert(target.clone())
                && upsert::upsert_relation(
                    conn,
                    *from_mod_id,
                    Some(*mod_version_id),
                    &target,
                    None,
                    RelKind::Requires,
                    None,
                    Source::Inferred,
                    now,
                )?
            {
                inferred_requires += 1;
            }
        }
    }
    // Optional pass: skip any target this same artifact already references hard.
    for (from_mod_id, mod_version_id, jar) in &derivations {
        for prefix in &jar.optional_refs {
            if let Some(target) = resolve_edge_target(conn, prefix, *from_mod_id)?
                && !hard_by_artifact
                    .get(mod_version_id)
                    .is_some_and(|h| h.contains(&target))
                && opt_by_artifact
                    .entry(*mod_version_id)
                    .or_default()
                    .insert(target.clone())
                && upsert::upsert_relation(
                    conn,
                    *from_mod_id,
                    Some(*mod_version_id),
                    &target,
                    None,
                    RelKind::OptionalDep,
                    None,
                    Source::Inferred,
                    now,
                )?
            {
                inferred_optional += 1;
            }
        }
    }

    for pack in &scan.packs {
        upsert::upsert_pack(conn, &pack.pack_id, now)?;
        let build = upsert::upsert_pack_build(
            conn,
            &pack.pack_id,
            &pack.pack_version,
            &pack.mc_version,
            pack.loader_id.as_deref(),
            pack.loader_version.as_deref(),
            pack.java_major,
            pack.fingerprint.as_deref(),
            true,
            now,
        )?;
        for bm in &pack.mods {
            if let Some(mv) = upsert::mod_version_id_for_sha1(conn, &bm.sha1)? {
                upsert::link_build_mod(
                    conn,
                    build,
                    mv,
                    &bm.filename,
                    bm.required,
                    bm.default_enabled,
                )?;
            }
        }
        // Curator conflicts: A's mod_id conflicts with B's modid (and reverse).
        // Left mod-level (no artifact scope): a pack author writing
        // `incompatible_with` is stating that the two mods do not get along, not
        // that one particular build does not -- so it should hold for whatever
        // artifact of the mod a pack ships.
        for (a_sha, b_sha) in &pack.conflicts {
            if let (Some(a_mod), Some(b_modid)) = (
                queries::mod_id_for_sha1(conn, a_sha)?,
                modid_by_sha.get(b_sha.as_str()),
            ) {
                upsert::upsert_relation(
                    conn,
                    a_mod,
                    None,
                    b_modid,
                    None,
                    RelKind::Conflicts,
                    // a curator writing `incompatible_with` means the hard one
                    Some(Severity::Hard),
                    Source::Curator,
                    now,
                )?;
            }
            if let (Some(b_mod), Some(a_modid)) = (
                queries::mod_id_for_sha1(conn, b_sha)?,
                modid_by_sha.get(a_sha.as_str()),
            ) {
                upsert::upsert_relation(
                    conn,
                    b_mod,
                    None,
                    a_modid,
                    None,
                    RelKind::Conflicts,
                    // a curator writing `incompatible_with` means the hard one
                    Some(Severity::Hard),
                    Source::Curator,
                    now,
                )?;
            }
        }
    }

    // Reconcile a mod split across identities that no single artifact bridges: a
    // Modrinth-identified mod whose slug equals another mod's forge modid is the same
    // mod (AutoRegLib re-hosted under modid `autoreglib`, its Modrinth twin carrying
    // slug `autoreglib`, on different jars). Fold the pair so a `modrinth:<project>`
    // dependency and a modid placement resolve to one mod. Which row survives does
    // not matter -- both identities land on it either way. DB-only, no egress.
    let split_pairs: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT a.mod_id, b.id, b.slug
             FROM mods b
             JOIN mod_alias mr ON mr.mod_id = b.id AND mr.source = 'modrinth'
             JOIN mod_alias a ON a.source = 'modid' AND a.external_key = b.slug COLLATE NOCASE
             WHERE b.slug IS NOT NULL AND b.slug != '' AND a.mod_id != b.id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut consumed: HashSet<i64> = HashSet::new();
    let mut identities_reconciled = 0i64;
    for (modid_mod, project_mod, slug) in split_pairs {
        if consumed.contains(&modid_mod) || consumed.contains(&project_mod) {
            continue;
        }
        let survivor = upsert::merge_collided_mods(conn, &[modid_mod, project_mod], now)?;
        let loser = if survivor == modid_mod {
            project_mod
        } else {
            modid_mod
        };
        let folded = conn
            .query_row("SELECT 1 FROM mods WHERE id = ?1", params![loser], |_| {
                Ok(())
            })
            .optional()?
            .is_none();
        if folded {
            consumed.insert(loser);
            identities_reconciled += 1;
            // audit trail: a slug==modid fold is a heuristic bridge, so name the
            // rows folded and the slug that matched, in case a coincidental match
            // ever unions two genuinely distinct mods.
            tracing::info!(
                slug = %slug,
                modid_mod,
                project_mod,
                survivor,
                "reconciled a slug-matched identity split"
            );
        }
    }

    // Link a self-hosted provider to the Modrinth project a dependency names. A
    // `modrinth:<project>` edge (from a Modrinth mod's declared deps) resolves only
    // against a mod carrying that project alias -- but the provider may be a jar the
    // mirror re-hosts under its forge modid, unlinked to the project (its repackaged
    // bytes are not on Modrinth by sha, so the hash lookup never tied them). Modrinth
    // identity has priority: a project some mod already owns is left alone. Otherwise
    // a mod whose modid matches the project slug is the same mod re-hosted, so attach
    // the project alias to it and the edge resolves.
    let mut modrinth_selfhost_links = 0i64;
    for (dep_pid, slug) in &scan.dep_project_slugs {
        if queries::mod_id_for_alias(conn, "modrinth", dep_pid)?.is_some() {
            continue;
        }
        if let Some(mod_id) = queries::mod_id_for_selector(conn, slug)? {
            let inserted = conn.execute(
                "INSERT INTO mod_alias (mod_id, source, external_key)
                 VALUES (?1, 'modrinth', ?2)
                 ON CONFLICT(source, external_key) DO NOTHING",
                params![mod_id, dep_pid],
            )?;
            modrinth_selfhost_links += inserted as i64;
        }
    }

    // External Modrinth deps (the hybrid case) resolve once every jar is
    // registered: the filename bridge needs the full artifact table, and the
    // identity folds above may have just settled the selector it lands on.
    for (from_mod, from_mv, kind, severity, fname) in &external_deps {
        let target =
            external_dep_selector(conn, fname)?.unwrap_or_else(|| format!("external:{fname}"));
        if upsert::upsert_relation(
            conn,
            *from_mod,
            Some(*from_mv),
            &target,
            None,
            *kind,
            *severity,
            Source::Modrinth,
            now,
        )? {
            modrinth_deps_written += 1;
        }
    }

    // Environment flags, applied per project through the alias -- after the
    // link/reconcile steps above so a self-hosted provider that just acquired
    // the project alias gets its flags in the same run.
    for (pid, (client, server)) in &scan.project_envs {
        if let Some(mod_id) = queries::mod_id_for_alias(conn, "modrinth", pid)? {
            upsert::set_mod_env_flags(
                conn,
                mod_id,
                (!client.is_empty()).then_some(client.as_str()),
                (!server.is_empty()).then_some(server.as_str()),
                now,
            )?;
        }
    }

    // drop the provisional 'unknown' releases left empty once files moved to
    // their channel / content-signature release
    upsert::prune_empty_releases(conn)?;

    let s = queries::stats(conn)?;
    Ok(HarvestReport {
        jars_scanned: scan.jars.len(),
        jars_no_identity: no_identity,
        mods: s.mods,
        mod_versions: s.mod_versions,
        relations: s.relations,
        packs: scan.packs.len(),
        builds: s.builds,
        inferred_requires,
        inferred_optional,
        sides_derived,
        match_policies_derived,
        non_mod_jars,
        modrinth_deps: modrinth_deps_written,
        declared_deps: declared_deps_written,
        modrinth_modids_learned: scan.modrinth_modids_learned,
        modrinth_selfhost_links,
        identities_reconciled,
        loader_bridges: bridges_written,
    })
}

/// Scan the storage tree + Modrinth into a [ScanData]. Async (FS reads + one
/// batched Modrinth lookup); does not touch the registry.
pub async fn scan(
    storage: &Storage,
    modrinth: &Modrinth,
    known_modid_projects: &HashSet<String>,
    known_project_aliases: &HashSet<String>,
    envless_project_aliases: &HashSet<String>,
) -> Result<ScanData> {
    let inventory = storage.list_cache_inventory().await.map_err(ae)?;
    let mut size_by_sha: HashMap<String, i64> = inventory
        .iter()
        .map(|e| (e.sha1.clone(), e.size_bytes as i64))
        .collect();
    // jars whose bytes are locally cached (read directly below); a Modrinth-only
    // mod's sha is absent here, which is how the modid-fetch pass finds it.
    let cache_shas: HashSet<String> = inventory.iter().map(|e| e.sha1.clone()).collect();
    let mut all_shas: HashSet<String> = inventory.iter().map(|e| e.sha1.clone()).collect();

    // read mcmod.info + derive jar facts (loader marker, content signature) +
    // scan bytecode (owned packages, references, side) from every cached jar
    // (Modrinth-only mods have no local jar to read)
    // Read + parse every cached jar OFF the async runtime: each is CPU-heavy (one
    // zip open decompressing every .class), so doing it inline would stall a tokio
    // worker and the panel behind it. Paths are resolved up front so the blocking
    // task owns its inputs and borrows nothing async.
    let jar_paths: Vec<(String, std::path::PathBuf)> = inventory
        .iter()
        .filter_map(|e| {
            storage
                .cache_jar_path(&e.sha1[..2], &e.sha1)
                .ok()
                .map(|p| (e.sha1.clone(), p))
        })
        .collect();
    let (
        mcmod_by_sha,
        facts_by_sha,
        bytecode_by_sha,
        modmeta_by_sha,
        extra_modids_by_sha,
        readout_by_sha,
    ) = tokio::task::spawn_blocking(move || {
        let mut mcmod: HashMap<String, McModInfo> = HashMap::new();
        let mut facts: HashMap<String, JarFacts> = HashMap::new();
        let mut bc: HashMap<String, bytecode::JarBytecode> = HashMap::new();
        let mut mm: HashMap<String, modmeta::ModMeta> = HashMap::new();
        let mut extra: HashMap<String, Vec<String>> = HashMap::new();
        let mut readouts: HashMap<String, MixinReadout> = HashMap::new();
        for (sha, path) in jar_paths {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let r = read_jar(&bytes);
            readouts.insert(
                sha.clone(),
                (
                    r.class_names,
                    r.required_mixins
                        .into_iter()
                        .flat_map(|c| c.needs.into_iter().map(move |n| (c.config.clone(), n)))
                        .collect(),
                ),
            );
            facts.insert(sha.clone(), r.facts);
            bc.insert(sha.clone(), r.bytecode);
            mm.insert(sha.clone(), r.modmeta);
            if r.mcmod_modids.len() > 1 {
                extra.insert(sha.clone(), r.mcmod_modids);
            }
            if let Some(info) = r.mcmod {
                mcmod.insert(sha.clone(), info);
            }
        }
        (mcmod, facts, bc, mm, extra, readouts)
    })
    .await
    .map_err(|e| anyhow::anyhow!("jar scan task: {e}"))?;

    // published builds + curator conflicts, per pack
    let mut packs = Vec::new();
    let mut filename_by_sha: HashMap<String, String> = HashMap::new();
    // Modrinth-source mods a build declares (project_id, version_id), keyed by sha.
    // A repackaged jar whose sha Modrinth does not recognize would otherwise get no
    // project identity from the sha1 lookup, leaving the pack's own Modrinth mod
    // (better-advancements, customskinloader) unresolved; the manifest names the
    // project directly, so register from it.
    let mut manifest_modrinth_by_sha: HashMap<String, (String, String)> = HashMap::new();
    for pid in storage.list_authoring_packs().await.map_err(ae)? {
        let Ok(manifest) = storage.load_latest_manifest(&pid).await else {
            continue; // unbuilt pack -> no published build to record
        };
        let mut mods = Vec::new();
        let mut sha_by_filename: HashMap<String, String> = HashMap::new();
        for m in &manifest.mods {
            all_shas.insert(m.sha1.clone());
            size_by_sha
                .entry(m.sha1.clone())
                .or_insert(m.size_bytes as i64);
            filename_by_sha
                .entry(m.sha1.clone())
                .or_insert_with(|| m.filename.clone());
            sha_by_filename.insert(m.filename.clone(), m.sha1.clone());
            mods.push(BuildModSeed {
                sha1: m.sha1.clone(),
                filename: m.filename.clone(),
                required: m.required,
                default_enabled: m.default_enabled,
            });
            if let crate::domain::manifest::Source::Modrinth {
                project_id,
                version_id,
            } = &m.source
            {
                manifest_modrinth_by_sha
                    .entry(m.sha1.clone())
                    .or_insert_with(|| (project_id.clone(), version_id.clone()));
            }
        }
        // conflicts come from each mod's published display.incompatible_with
        // (set per-mod in the config now, not a curator table). sha_by_filename is
        // complete from the loop above, so a target naming a later mod resolves.
        let mut conflicts = Vec::new();
        for m in &manifest.mods {
            let Some(disp) = &m.display else { continue };
            for b_fname in &disp.incompatible_with {
                if let Some(b_sha) = sha_by_filename.get(b_fname) {
                    conflicts.push((m.sha1.clone(), b_sha.clone()));
                }
            }
        }
        packs.push(PackSeed {
            pack_id: pid,
            pack_version: manifest.pack_version.clone(),
            mc_version: manifest.minecraft.version.clone(),
            loader_id: Some(manifest.loader.name.clone()),
            loader_version: Some(manifest.loader.version.clone()),
            java_major: Some(manifest.java.major as i64),
            fingerprint: manifest.fingerprint.clone(),
            mods,
            conflicts,
        });
    }

    // one batched identity lookup over every sha1 we know
    let sha_vec: Vec<String> = all_shas.iter().cloned().collect();
    let (modrinth_by_sha, modrinth_leg_ok) = match modrinth.version_files_by_sha1(&sha_vec).await {
        Ok(m) if m.is_empty() && !sha_vec.is_empty() => {
            // a degraded upstream can answer 200 with nothing matched; on a
            // mirror that has ever matched anything, believing that would
            // wipe the modrinth relation layer until the next healthy run
            tracing::warn!(
                hashes = sha_vec.len(),
                "modrinth sha lookup returned no matches; treating the leg as degraded"
            );
            (m, false)
        }
        Ok(m) => (m, true),
        Err(e) => {
            tracing::warn!(error = %e, "modrinth lookup failed; harvesting jar-meta only");
            (HashMap::new(), false)
        }
    };

    // enrich metadata for Modrinth-identified jars: one batched project lookup
    // (title, slug, team, environment flags), then one batched team lookup for
    // the owner username. Both degrade to empty on failure -- jar-meta still
    // fills name/author where present, identity harvest is unaffected.
    //
    // The project batch covers EVERY identified project: only the project
    // object carries the client_side/server_side environment flags, and those
    // are the priority-1 side source for classification. Egress note: the ids
    // sent here already reached Modrinth in the sha1 lookup above, so no new
    // information leaves the machine. The team (author) lookup keeps the
    // narrow privacy gate: only projects whose jar-meta lacks a name or author.
    let needs_enrich = |sha: &str| match mcmod_by_sha.get(sha) {
        Some(i) => i.name.trim().is_empty() || i.authors.is_empty(),
        None => true,
    };
    let enrich_projects: HashSet<String> = modrinth_by_sha
        .iter()
        .filter(|(sha, _)| needs_enrich(sha))
        .map(|(_, v)| v.project_id.clone())
        .collect();
    let project_ids: Vec<String> = modrinth_by_sha
        .values()
        .map(|v| v.project_id.clone())
        .chain(manifest_modrinth_by_sha.values().map(|(p, _)| p.clone()))
        // aliased projects still missing env flags (an alias the self-host slug
        // bridge attached after the fact, or rows predating the env columns)
        .chain(envless_project_aliases.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let projects = match modrinth.projects_by_ids(&project_ids).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "modrinth project lookup failed; names/slugs from jar-meta only");
            HashMap::new()
        }
    };

    // Dependency-target projects whose slug write_scan matches against a self-hosted
    // modid, to link a `modrinth:<project>` dependency to a provider the mirror
    // re-hosts. Skip a project some mod already owns -- a linked or Modrinth-native
    // one needs no lookup, so this shrinks to nothing once a mirror is warm. Kept
    // out of the enrichment batch above so it never amplifies the team lookup.
    let dep_projects: Vec<String> = modrinth_by_sha
        .values()
        .flat_map(|v| &v.dependencies)
        .filter_map(|d| d.project_id.clone())
        .filter(|p| !p.is_empty() && !known_project_aliases.contains(p))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let dep_project_objects: HashMap<String, Project> = if dep_projects.is_empty() {
        HashMap::new()
    } else {
        match modrinth.projects_by_ids(&dep_projects).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "modrinth dep-project slug lookup failed; self-host links skipped");
                HashMap::new()
            }
        }
    };
    let dep_project_slugs: HashMap<String, String> = dep_project_objects
        .iter()
        .filter(|(_, p)| !p.slug.is_empty())
        .map(|(pid, p)| (pid.clone(), p.slug.clone()))
        .collect();

    // Environment flags of every project object this scan saw, keyed by project
    // id; write_scan lands them through the alias once identities are settled.
    let mut project_envs: HashMap<String, (String, String)> = HashMap::new();
    for p in projects.values().chain(dep_project_objects.values()) {
        if !p.client_side.is_empty() || !p.server_side.is_empty() {
            project_envs.insert(p.id.clone(), (p.client_side.clone(), p.server_side.clone()));
        }
    }
    let team_ids: Vec<String> = projects
        .values()
        .filter(|p| enrich_projects.contains(&p.id))
        .map(|p| p.team.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let team_owners = match modrinth.team_owners_by_ids(&team_ids).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "modrinth team lookup failed; authors from jar-meta only");
            HashMap::new()
        }
    };

    // Learn the forge modid of a Modrinth re-upload (a mod the pack ships from
    // Modrinth, so its bytes are not in the local cache) by fetching its jar once
    // and reading the modid. Without this, a dependency keyed on that modid (an
    // IC2 addon requiring `ic2`) can never resolve, because the registry knows the
    // re-upload only by its Modrinth project id. Skipped once a modid alias exists
    // for the project, so the fetch is a one-time cost per mod, not per harvest.
    let mut learned_modid: HashMap<String, String> = HashMap::new();
    let fetch_targets: Vec<(String, String)> = modrinth_by_sha
        .iter()
        .filter(|(sha, _)| !cache_shas.contains(sha.as_str()))
        .filter(|(_, v)| !known_modid_projects.contains(&v.project_id))
        .filter_map(|(sha, v)| v.primary_file().map(|f| (sha.clone(), f.url.clone())))
        .collect();
    for (sha, url) in fetch_targets {
        let bytes = match modrinth.fetch_bytes(&url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, url = %url, "modrinth jar fetch failed; modid unlearned");
                continue;
            }
        };
        let modid = tokio::task::spawn_blocking(move || {
            let r = read_jar(&bytes);
            r.mcmod
                .map(|i| i.modid)
                .filter(|s| !s.is_empty())
                .or(r.modmeta.modid)
                .or(r.bytecode.mod_id)
        })
        .await
        .map_err(|e| anyhow::anyhow!("modid read task: {e}"))?;
        if let Some(modid) = modid {
            learned_modid.insert(sha, modid);
        }
    }
    let modrinth_modids_learned = learned_modid.len();

    let jars = all_shas
        .into_iter()
        .map(|sha| {
            let info = mcmod_by_sha.get(&sha);
            let mrv = modrinth_by_sha.get(&sha);
            let facts = facts_by_sha.get(&sha);
            let bc = bytecode_by_sha.get(&sha);
            let mm = modmeta_by_sha.get(&sha);
            // Modrinth identity: the sha1 match, else the project the manifest
            // itself declares (a repackaged jar Modrinth does not know by hash).
            let project_id = mrv
                .map(|v| v.project_id.clone())
                .or_else(|| manifest_modrinth_by_sha.get(&sha).map(|(p, _)| p.clone()));
            let project = project_id.as_deref().and_then(|p| projects.get(p));
            // name: jar-meta name wins (local: mcmod.info, else the modern
            // declared displayName), else Modrinth title
            let name = info
                .map(|i| i.name.clone())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| mm.and_then(|m| m.display_name.clone()))
                .or_else(|| {
                    project
                        .map(|p| p.title.clone())
                        .filter(|s| !s.trim().is_empty())
                });
            // author: jar-meta authorList wins (local), else the project's team owner
            let author = info
                .map(|i| i.authors.join(", "))
                .filter(|s| !s.trim().is_empty())
                .or_else(|| project.and_then(|p| team_owners.get(&p.team).cloned()));
            // slug is a Modrinth concept; jars carry none
            let slug = project.map(|p| p.slug.clone()).filter(|s| !s.is_empty());
            JarSeed {
                size_bytes: size_by_sha.get(&sha).copied().unwrap_or(0),
                // identity: jar-meta modid wins; else the modern declared modid
                // (mods.toml/fabric.mod.json); else the class-level @Mod annotation
                // modid, so an old Forge jar carrying neither metadata file (Chisel,
                // HatStand) is not identity-less and stays invisible on the mirror.
                modid: info
                    .map(|i| i.modid.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| mm.and_then(|m| m.modid.clone()))
                    .or_else(|| bc.and_then(|b| b.mod_id.clone()))
                    .or_else(|| learned_modid.get(&sha).cloned()),
                extra_modids: extra_modids_by_sha.get(&sha).cloned().unwrap_or_default(),
                version: info
                    .map(|i| i.version.clone())
                    .filter(|s| !s.is_empty())
                    .or_else(|| mm.and_then(|m| m.version.clone()))
                    .or_else(|| mrv.map(|v| v.version_number.clone())),
                project_id,
                // loader: Modrinth's set wins; else the jar's own marker
                // (mcmod.info/mods.toml -> forge, fabric.mod.json -> fabric); else
                // empty (-> 'any' downstream)
                loaders: match mrv.map(|v| v.loaders.clone()).filter(|l| !l.is_empty()) {
                    Some(l) => l,
                    None => facts.and_then(|f| f.loader.clone()).into_iter().collect(),
                },
                // mc: Modrinth's set wins; else the jar's declared mcversion when
                // it looks like a real version (not a gradle token); else the
                // modern manifest's minecraft dependency lower bound
                mc_versions: match mrv
                    .map(|v| v.game_versions.clone())
                    .filter(|g| !g.is_empty())
                {
                    Some(g) => g,
                    None => info
                        .and_then(|i| clean_mc_version(&i.mcversion))
                        .or_else(|| mm.and_then(|m| m.mc.clone()))
                        .into_iter()
                        .collect(),
                },
                requires: info.map(mcmod_hard_deps).unwrap_or_default(),
                filename: filename_by_sha.get(&sha).cloned(),
                name,
                author,
                slug,
                modrinth_version_id: mrv
                    .map(|v| v.id.clone())
                    .or_else(|| manifest_modrinth_by_sha.get(&sha).map(|(_, v)| v.clone())),
                channel: mrv.and_then(|v| channel_from_version_type(&v.version_type)),
                owned_packages: bc
                    .map(|b| b.owned.iter().cloned().collect())
                    .unwrap_or_default(),
                class_names: readout_by_sha
                    .get(&sha)
                    .map(|(c, _)| c.clone())
                    .unwrap_or_default(),
                mixin_needs: readout_by_sha
                    .get(&sha)
                    .map(|(_, m)| m.clone())
                    .unwrap_or_default(),
                hard_refs: bc
                    .map(|b| b.hard_refs.iter().cloned().collect())
                    .unwrap_or_default(),
                optional_refs: bc
                    .map(|b| b.optional_refs.iter().cloned().collect())
                    .unwrap_or_default(),
                side: bc.and_then(|b| b.side).map(|s| s.as_str().to_string()),
                side_confidence: bc
                    .and_then(|b| b.side_confidence)
                    .map(|c| c.as_str().to_string()),
                match_policy: bc
                    .and_then(|b| b.match_policy)
                    .map(|p| p.as_str().to_string()),
                kind: bc.and_then(|b| b.kind).map(|k| k.as_str().to_string()),
                // Modrinth's declared deps, keeping only those naming a target
                // project (a dependency may carry only a version id -- skip those);
                // a pinned version_id rides along as the exact-version constraint.
                modrinth_deps: mrv
                    .map(|v| {
                        v.dependencies
                            .iter()
                            .filter(|d| d.project_id.is_some() || d.file_name.is_some())
                            .map(|d| ModrinthDepSeed {
                                project_id: d.project_id.clone(),
                                dep_type: d.dependency_type.clone(),
                                version_id: d.version_id.clone(),
                                file_name: d.file_name.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                loader_reqs: mm.map(|m| {
                    m.loader_reqs
                        .iter()
                        .map(|r| (r.loader.clone(), r.range.clone()))
                        .collect()
                }),
                declared_deps: mm
                    .map(|m| {
                        m.deps
                            .iter()
                            .map(|d| (d.modid.clone(), d.kind, d.severity, d.version_range.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                needs_runtime: bc
                    .map(|b| b.needs_runtime.iter().cloned().collect())
                    .unwrap_or_default(),
                provides_runtime: bc
                    .map(|b| b.provides_runtime.iter().cloned().collect())
                    .unwrap_or_default(),
                sha1: sha,
            }
        })
        .collect();

    Ok(ScanData {
        jars,
        packs,
        modrinth_modids_learned,
        dep_project_slugs,
        project_envs,
        modrinth_leg_ok,
    })
}

/// Full harvest: scan (async) then write (in a blocking transaction).
pub async fn run_harvest(
    storage: &Storage,
    modrinth: &Modrinth,
    registry: Arc<Registry>,
) -> Result<HarvestReport> {
    // Modrinth projects whose mod already carries a forge modid alias -- their jar
    // was read once before, so the scan skips re-fetching it (the one-time cost).
    let reg = registry.clone();
    let (known_modid_projects, known_project_aliases, envless_project_aliases) =
        tokio::task::spawn_blocking(move || {
            reg.with_conn(|c| {
                Ok((
                    queries::modrinth_projects_with_modid(c)?,
                    queries::modrinth_project_aliases(c)?,
                    queries::modrinth_aliases_without_env(c)?,
                ))
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("known-modid query task: {e}"))??;
    let scan = scan(
        storage,
        modrinth,
        &known_modid_projects,
        &known_project_aliases,
        &envless_project_aliases,
    )
    .await?;
    let now = upsert::now_rfc3339();
    let report =
        tokio::task::spawn_blocking(move || registry.with_txn(|c| write_scan(c, &scan, &now)))
            .await
            .map_err(|e| anyhow::anyhow!("harvest write task: {e}"))??;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ScanData {
        ScanData {
            jars: vec![
                JarSeed {
                    sha1: "sha_a".into(),
                    size_bytes: 100,
                    channel: None,
                    modid: Some("appleskin".into()),
                    extra_modids: vec![],
                    version: Some("2.5".into()),
                    project_id: Some("EsAfb37o".into()),
                    loaders: vec!["forge".into()],
                    mc_versions: vec!["1.12.2".into()],
                    requires: vec![],
                    filename: Some("appleskin.jar".into()),
                    name: Some("AppleSkin".into()),
                    author: Some("squeek502".into()),
                    slug: Some("appleskin".into()),
                    modrinth_version_id: Some("mrv_apple".into()),
                    owned_packages: vec![],
                    class_names: vec![],
                    mixin_needs: vec![],
                    hard_refs: vec![],
                    optional_refs: vec![],
                    side: None,
                    side_confidence: None,
                    match_policy: None,
                    kind: None,
                    modrinth_deps: vec![],
                    declared_deps: vec![],
                    loader_reqs: None,
                    needs_runtime: vec![],
                    provides_runtime: vec![],
                },
                JarSeed {
                    sha1: "sha_b".into(),
                    size_bytes: 200,
                    channel: None,
                    modid: Some("jei".into()),
                    extra_modids: vec![],
                    version: Some("4.16".into()),
                    project_id: None,
                    loaders: vec!["forge".into()],
                    mc_versions: vec![],
                    requires: vec!["appleskin".into(), "forge".into()], // 'forge' filtered upstream
                    filename: Some("jei.jar".into()),
                    name: None,
                    author: None,
                    slug: None,
                    modrinth_version_id: None,
                    owned_packages: vec![],
                    class_names: vec![],
                    mixin_needs: vec![],
                    hard_refs: vec![],
                    optional_refs: vec![],
                    side: None,
                    side_confidence: None,
                    match_policy: None,
                    kind: None,
                    modrinth_deps: vec![],
                    declared_deps: vec![],
                    loader_reqs: None,
                    needs_runtime: vec![],
                    provides_runtime: vec![],
                },
                JarSeed {
                    sha1: "sha_noid".into(),
                    size_bytes: 50,
                    channel: None,
                    modid: None,
                    extra_modids: vec![],
                    version: None,
                    project_id: None,
                    loaders: vec![],
                    mc_versions: vec![],
                    requires: vec![],
                    filename: Some("mystery.jar".into()),
                    name: None,
                    author: None,
                    slug: None,
                    modrinth_version_id: None,
                    owned_packages: vec![],
                    class_names: vec![],
                    mixin_needs: vec![],
                    hard_refs: vec![],
                    optional_refs: vec![],
                    side: None,
                    side_confidence: None,
                    match_policy: None,
                    kind: None,
                    modrinth_deps: vec![],
                    declared_deps: vec![],
                    loader_reqs: None,
                    needs_runtime: vec![],
                    provides_runtime: vec![],
                },
            ],
            packs: vec![PackSeed {
                pack_id: "Industrial".into(),
                pack_version: "2026.06.06".into(),
                mc_version: "1.12.2".into(),
                loader_id: Some("forge".into()),
                loader_version: Some("14.23".into()),
                java_major: Some(8),
                fingerprint: Some("fp_test".into()),
                mods: vec![
                    BuildModSeed {
                        sha1: "sha_a".into(),
                        filename: "appleskin.jar".into(),
                        required: true,
                        default_enabled: true,
                    },
                    BuildModSeed {
                        sha1: "sha_b".into(),
                        filename: "jei.jar".into(),
                        required: true,
                        default_enabled: true,
                    },
                ],
                conflicts: vec![("sha_a".into(), "sha_b".into())],
            }],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        }
    }

    #[test]
    fn write_scan_harvests_idempotently_and_preserves_authored() {
        let r = Registry::open_in_memory().unwrap();
        let scan = sample();

        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(
            rep.jars_no_identity, 1,
            "the no-modid no-modrinth jar is skipped"
        );
        assert_eq!(rep.mod_versions, 2);

        r.with_conn(|c| {
            // modid + project_id collapse to one identity
            let by_modid = queries::mod_id_for_alias(c, "modid", "appleskin")?;
            let by_proj = queries::mod_id_for_alias(c, "modrinth", "EsAfb37o")?;
            assert!(by_modid.is_some() && by_modid == by_proj);
            // write_scan stores requires verbatim; pseudo-dep filtering happens
            // in scan() (covered by filter_deps_drops_platform_pseudo_deps), so
            // the sample's ["appleskin","forge"] both land here.
            let req: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE kind='requires'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(req, 2);
            // conflict recorded both directions
            let conf: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE kind='conflicts'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(conf, 2);
            // the build's content fingerprint lands in pack_build
            let fp: Option<String> = c.query_row(
                "SELECT fingerprint FROM pack_build WHERE pack_id='Industrial'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(fp.as_deref(), Some("fp_test"));
            // human metadata from the seed lands on the mods row
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let (name, author, slug): (Option<String>, Option<String>, Option<String>) = c
                .query_row(
                    "SELECT canonical_name, author, slug FROM mods WHERE id = ?1",
                    [apple],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
            assert_eq!(name.as_deref(), Some("AppleSkin"));
            assert_eq!(author.as_deref(), Some("squeek502"));
            assert_eq!(slug.as_deref(), Some("appleskin"));
            let mrv: Option<String> = c.query_row(
                "SELECT modrinth_version_id FROM mod_version WHERE sha1 = 'sha_a'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(mrv.as_deref(), Some("mrv_apple"));
            Ok(())
        })
        .unwrap();

        // idempotent re-run
        let rep2 = r.with_txn(|c| write_scan(c, &scan, "T1")).unwrap();
        assert_eq!(rep2.mods, rep.mods);
        assert_eq!(rep2.mod_versions, rep.mod_versions);
        assert_eq!(rep2.relations, rep.relations);
        assert_eq!(rep2.builds, rep.builds);

        // promote a row to authored, re-harvest, confirm it survives untouched
        r.with_txn(|c| {
            c.execute(
                "UPDATE mod_version SET source='authored', version='AUTH' WHERE sha1='sha_b'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        r.with_txn(|c| write_scan(c, &scan, "T2")).unwrap();
        r.with_conn(|c| {
            let v: String = c.query_row(
                "SELECT version FROM mod_version WHERE sha1='sha_b'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(v, "AUTH");
            Ok(())
        })
        .unwrap();
    }

    // scan() learned a Modrinth re-upload's forge modid by fetching its jar (IC2,
    // shipped from Modrinth, bytes not in the local cache): the seed then carries
    // both the modid and the project id. write_scan must fold them into one mod so
    // an addon's modid-keyed dependency (`ic2`) resolves to the Modrinth mod, and
    // the project is thereafter known so the fetch never repeats.
    #[test]
    fn write_scan_merges_learned_modid_onto_modrinth_mod() {
        let r = Registry::open_in_memory().unwrap();
        let mut ic2 = jar("sha_ic2", "ic2", Some("2.8"), vec!["forge".into()]);
        ic2.project_id = Some("wTncj5gs".into());
        let mut addon = jar("sha_addon", "advmachines", Some("1"), vec!["forge".into()]);
        addon.requires = vec!["ic2".into()];
        let scan = ScanData {
            jars: vec![ic2, addon],
            packs: vec![],
            modrinth_modids_learned: 1,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let by_modid =
                queries::mod_id_for_selector(c, "ic2")?.expect("ic2 selector resolves to a mod");
            let by_project = queries::mod_id_for_alias(c, "modrinth", "wTncj5gs")?
                .expect("the modrinth project alias exists");
            assert_eq!(
                by_modid, by_project,
                "the forge modid and the modrinth project id name the same mod"
            );
            let known = queries::modrinth_projects_with_modid(c)?;
            assert!(
                known.contains("wTncj5gs"),
                "the project now carries a modid alias, so the fetch is skipped next harvest"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn write_scan_links_selfhosted_provider_to_a_modrinth_dep_project() {
        // Quark depends on AutoRegLib by its Modrinth project (NvZ9ZhwE); the mirror
        // re-hosts AutoRegLib as a self-hosted jar under modid `autoreglib`, unlinked
        // to that project. The scan resolved the project slug; write_scan attaches the
        // project alias to the self-hosted mod so the modrinth:<project> dep resolves.
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![jar(
                "sha_arl",
                "autoreglib",
                Some("1"),
                vec!["forge".into()],
            )],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: HashMap::from([("NvZ9ZhwE".to_string(), "autoreglib".to_string())]),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.modrinth_selfhost_links, 1);
        r.with_conn(|c| {
            let by_modid = queries::mod_id_for_selector(c, "autoreglib")?.unwrap();
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:NvZ9ZhwE")?,
                Some(by_modid),
                "the project-keyed dep now resolves to the self-hosted provider"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn write_scan_reconciles_a_slug_matched_identity_split() {
        // AutoRegLib is two rows no single artifact bridges: a cache jar under modid
        // `autoreglib`, and a Modrinth twin (a different jar) carrying project
        // NvZ9ZhwE plus slug `autoreglib`. The slug==modid match must fold them so a
        // modrinth:<project> dependency and a modid placement resolve to one mod.
        let r = Registry::open_in_memory().unwrap();
        let cache = jar("sha_cache", "autoreglib", Some("1.6"), vec!["forge".into()]);
        let mut twin = jar("sha_twin", "unused", Some("1.6"), vec!["forge".into()]);
        twin.modid = None;
        twin.project_id = Some("NvZ9ZhwE".into());
        twin.slug = Some("autoreglib".into());
        let scan = ScanData {
            jars: vec![cache, twin],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let by_modid = queries::mod_id_for_selector(c, "autoreglib")?.unwrap();
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:NvZ9ZhwE")?,
                Some(by_modid),
                "the slug-matched split folded to one mod"
            );
            let mods: i64 = c.query_row("SELECT count(*) FROM mods", [], |r| r.get(0))?;
            assert_eq!(mods, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn selfhost_link_yields_to_an_existing_modrinth_owner() {
        // Modrinth identity has priority: when a real Modrinth mod already owns the
        // project, the self-hosted modid mod is not relinked to it.
        let r = Registry::open_in_memory().unwrap();
        let existing = r
            .with_txn(|c| {
                let m = upsert::upsert_mod_by_alias(c, &[("modrinth", "NvZ9ZhwE")], "T0")?;
                upsert::upsert_mod_version(c, m, "1", &["forge"], "sha_real", 1, None, None, "T0")?;
                Ok(m)
            })
            .unwrap();
        let scan = ScanData {
            jars: vec![jar(
                "sha_arl",
                "autoreglib",
                Some("1"),
                vec!["forge".into()],
            )],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: HashMap::from([("NvZ9ZhwE".to_string(), "autoreglib".to_string())]),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T1")).unwrap();
        assert_eq!(
            rep.modrinth_selfhost_links, 0,
            "no self-host link is made while a Modrinth owner exists"
        );
        r.with_conn(|c| {
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:NvZ9ZhwE")?,
                Some(existing),
                "the project still resolves to the Modrinth mod"
            );
            let arl = queries::mod_id_for_selector(c, "autoreglib")?.unwrap();
            assert_ne!(arl, existing, "the self-hosted mod stays a distinct row");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_degraded_modrinth_leg_keeps_the_last_good_relations() {
        let r = Registry::open_in_memory().unwrap();
        // healthy run first: sample's appleskin/jei land, including a modrinth
        // relation seeded by hand on top of it
        let scan = sample();
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn_mut(|c| {
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            upsert::upsert_relation(
                c,
                apple,
                None,
                "somedep",
                None,
                crate::registry::model::RelKind::Requires,
                None,
                crate::registry::model::Source::Modrinth,
                "T0",
            )?;
            Ok(())
        })
        .unwrap();
        let count = |r: &Registry, src: &str| -> i64 {
            r.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM relation WHERE source = ?1",
                    [src],
                    |row| row.get(0),
                )?)
            })
            .unwrap()
        };
        assert_eq!(count(&r, "modrinth"), 1);

        // degraded run: the modrinth layer survives untouched
        let mut degraded = sample();
        degraded.modrinth_leg_ok = false;
        r.with_txn(|c| write_scan(c, &degraded, "T1")).unwrap();
        assert_eq!(
            count(&r, "modrinth"),
            1,
            "upstream weather must not erase the modrinth relation layer"
        );

        // healthy run again: the layer is authoritative and rewritten (the
        // hand-seeded edge is not in the scan, so it goes away)
        r.with_txn(|c| write_scan(c, &sample(), "T2")).unwrap();
        assert_eq!(count(&r, "modrinth"), 0);
    }

    #[test]
    fn write_scan_registers_every_modid_a_bundled_jar_declares() {
        // ForgeMultipart ships three mods in one jar (forgemultipartcbe +
        // minecraftmultipartcbe + microblockcbe); a dependency on any bundled modid
        // must resolve to the jar that provides it, so each becomes an alias.
        let r = Registry::open_in_memory().unwrap();
        let mut fmp = jar(
            "sha_fmp",
            "forgemultipartcbe",
            Some("2.6"),
            vec!["forge".into()],
        );
        fmp.extra_modids = vec![
            "forgemultipartcbe".into(),
            "minecraftmultipartcbe".into(),
            "microblockcbe".into(),
        ];
        let scan = ScanData {
            jars: vec![fmp],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let primary = queries::mod_id_for_selector(c, "forgemultipartcbe")?.unwrap();
            assert_eq!(
                queries::mod_id_for_selector(c, "minecraftmultipartcbe")?,
                Some(primary),
                "a bundled modid resolves to the jar's mod"
            );
            assert_eq!(
                queries::mod_id_for_selector(c, "microblockcbe")?,
                Some(primary)
            );
            let mods: i64 = c.query_row("SELECT count(*) FROM mods", [], |r| r.get(0))?;
            assert_eq!(mods, 1, "all three bundled modids land on one mod");
            Ok(())
        })
        .unwrap();
    }

    fn jar(sha: &str, modid: &str, version: Option<&str>, loaders: Vec<String>) -> JarSeed {
        JarSeed {
            sha1: sha.into(),
            size_bytes: 1,
            channel: None,
            modid: Some(modid.into()),
            extra_modids: vec![],
            version: version.map(Into::into),
            project_id: None,
            loaders,
            mc_versions: vec![],
            requires: vec![],
            filename: Some(format!("{sha}.jar")),
            name: None,
            author: None,
            slug: None,
            modrinth_version_id: None,
            owned_packages: vec![],
            class_names: vec![],
            mixin_needs: vec![],
            hard_refs: vec![],
            optional_refs: vec![],
            side: None,
            side_confidence: None,
            match_policy: None,
            kind: None,
            modrinth_deps: vec![],
            declared_deps: vec![],
            loader_reqs: None,
            needs_runtime: vec![],
            provides_runtime: vec![],
        }
    }

    /// A jar seed carrying bytecode-derived facts, for the derivation tests.
    fn dseed(
        sha: &str,
        modid: &str,
        owned: &[&str],
        hard: &[&str],
        opt: &[&str],
        side: Option<&str>,
    ) -> JarSeed {
        let strs = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        JarSeed {
            sha1: sha.into(),
            size_bytes: 1,
            channel: None,
            modid: Some(modid.into()),
            extra_modids: vec![],
            version: Some("1".into()),
            project_id: None,
            loaders: vec!["forge".into()],
            mc_versions: vec![],
            requires: vec![],
            filename: Some(format!("{sha}.jar")),
            name: None,
            author: None,
            slug: None,
            modrinth_version_id: None,
            owned_packages: strs(owned),
            class_names: vec![],
            mixin_needs: vec![],
            hard_refs: strs(hard),
            optional_refs: strs(opt),
            side: side.map(String::from),
            side_confidence: side.map(|_| "high".to_string()),
            match_policy: None,
            kind: Some("mod".into()),
            modrinth_deps: vec![],
            declared_deps: vec![],
            loader_reqs: None,
            needs_runtime: vec![],
            provides_runtime: vec![],
        }
    }

    /// A Modrinth-identified jar seed carrying `version.dependencies`
    /// (project_id, dependency_type, optional pinned version_id).
    fn mseed(
        sha: &str,
        modid: &str,
        project_id: &str,
        deps: &[(&str, &str, Option<&str>)],
    ) -> JarSeed {
        let mut s = dseed(sha, modid, &[], &[], &[], None);
        s.project_id = Some(project_id.into());
        s.modrinth_deps = deps
            .iter()
            .map(|(p, t, v)| ModrinthDepSeed {
                project_id: Some(p.to_string()),
                dep_type: t.to_string(),
                version_id: v.map(String::from),
                file_name: None,
            })
            .collect();
        s
    }

    // The bridge fact the resolver needs but nobody ever wrote: harvesting the
    // connector emits `provides loader:fabric` off the shipped bridge table, so a
    // fabric jar in a neoforge pack reads as carried instead of dead (#50).
    #[test]
    fn write_scan_emits_the_loader_capability_of_a_known_bridge() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            // u58R1TMW is the seeded Sinytra Connector project
            jars: vec![mseed("sha_conn", "connector", "u58R1TMW", &[])],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(
            rep.loader_bridges, 1,
            "the connector declares what it carries"
        );

        let edges = r
            .with_conn(|c| {
                let id = queries::mod_id_for_alias(c, "modrinth", "u58R1TMW")?.unwrap();
                queries::relations_from(c, id)
            })
            .unwrap();
        let provides: Vec<&str> = edges
            .iter()
            .filter(|e| e.kind == RelKind::Provides)
            .map(|e| e.target.as_str())
            .collect();
        assert_eq!(provides, vec!["loader:fabric"]);

        // a mod the table does not name gets no capability invented for it
        let plain = ScanData {
            jars: vec![mseed("sha_plain", "plain", "SOMEPROJ", &[])],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &plain, "T1")).unwrap();
        assert_eq!(rep.loader_bridges, 0);
    }

    // The scala/kotlin classpath need STOP_PREFIXES hides from the edge graph:
    // a jar that references the runtime emits `requires runtime:<lang>`, its
    // carrier emits `provides runtime:<lang>`, and the resolver's existing
    // capability pass reconciles them -- reporting the gap when no carrier is in
    // the pack (the Scala-on-Cleanroom crash, caught before boot).
    #[test]
    fn write_scan_emits_runtime_capability_edges() {
        let r = Registry::open_in_memory().unwrap();
        let mut needer = dseed("sha_bdlib", "bdlib", &["net/bdew/lib"], &[], &[], None);
        needer.needs_runtime = vec!["scala".into()];
        let mut carrier = dseed(
            "sha_scalar",
            "scalar",
            &["com/cleanroommc/scalar"],
            &[],
            &[],
            None,
        );
        carrier.provides_runtime = vec!["scala".into()];
        let scan = ScanData {
            jars: vec![needer, carrier],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();

        let edges = |modid: &str| {
            r.with_conn(|c| {
                let id = queries::mod_id_for_alias(c, "modid", modid)?.unwrap();
                queries::relations_from(c, id)
            })
            .unwrap()
        };
        assert!(
            edges("bdlib")
                .iter()
                .any(|e| e.kind == RelKind::Requires && e.target == "runtime:scala"),
            "the scala mod requires the runtime capability"
        );
        assert!(
            edges("scalar")
                .iter()
                .any(|e| e.kind == RelKind::Provides && e.target == "runtime:scala"),
            "the carrier provides it"
        );
    }

    /// A non-Modrinth jar seed carrying modern declared deps (modid, kind, range).
    fn ddseed(sha: &str, modid: &str, deps: &[(&str, RelKind, Option<&str>)]) -> JarSeed {
        let mut s = dseed(sha, modid, &[], &[], &[], None);
        s.declared_deps = deps
            .iter()
            .map(|(m, k, v)| (m.to_string(), *k, None, v.map(String::from)))
            .collect();
        s
    }

    // The core of #40: owned packages populate the index, references resolve to
    // inferred requires/optional_dep edges, a jar's own reference makes no
    // self-edge, and the derived side lands on the artifact.
    #[test]
    fn write_scan_derives_inferred_edges_and_side() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![
                dseed(
                    "sha_ae2",
                    "appliedenergistics2",
                    &["appeng/api", "appeng/core"],
                    &["appeng/api"], // own package -> must NOT self-edge
                    &[],
                    Some("both"),
                ),
                dseed(
                    "sha_stuff",
                    "ae2stuff",
                    &["ae2stuff/block"],
                    &["appeng/api"],
                    &[],
                    None,
                ),
                dseed("sha_jei", "jei", &["mezz/jei"], &[], &[], None),
                dseed(
                    "sha_jeibees",
                    "jeibees",
                    &["jeibees/core"],
                    &[],
                    &["mezz/jei"],
                    None,
                ),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };

        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(
            rep.inferred_requires, 1,
            "ae2stuff -> AE2 (AE2's self-ref skipped)"
        );
        assert_eq!(rep.inferred_optional, 1, "jeibees -> JEI (optional)");
        assert_eq!(rep.sides_derived, 1);

        r.with_conn(|c| {
            let stuff = queries::mod_id_for_alias(c, "modid", "ae2stuff")?.unwrap();
            let (target, kind, source): (String, String, String) = c.query_row(
                "SELECT target_modid, kind, source FROM relation WHERE from_mod_id = ?1",
                [stuff],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            assert_eq!(
                (target.as_str(), kind.as_str(), source.as_str()),
                ("appliedenergistics2", "requires", "inferred")
            );

            let jeibees = queries::mod_id_for_alias(c, "modid", "jeibees")?.unwrap();
            let (t2, k2): (String, String) = c.query_row(
                "SELECT target_modid, kind FROM relation WHERE from_mod_id = ?1",
                [jeibees],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((t2.as_str(), k2.as_str()), ("jei", "optional_dep"));

            let ae2 = queries::mod_id_for_alias(c, "modid", "appliedenergistics2")?.unwrap();
            let self_edges: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE from_mod_id = ?1",
                [ae2],
                |r| r.get(0),
            )?;
            assert_eq!(
                self_edges, 0,
                "no self-dependency from an own-package reference"
            );

            let (kind, side): (String, Option<String>) = c.query_row(
                "SELECT kind, side FROM jar_class WHERE sha1 = 'sha_ae2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((kind.as_str(), side.as_deref()), ("mod", Some("both")));
            assert_eq!(queries::owner_mod_for_prefix(c, "appeng/api")?, Some(ae2));
            Ok(())
        })
        .unwrap();

        // idempotent: inferred edges are wiped and rebuilt, never duplicated
        let rep2 = r.with_txn(|c| write_scan(c, &scan, "T1")).unwrap();
        assert_eq!(rep2.inferred_requires, 1);
        assert_eq!(rep2.inferred_optional, 1);
        r.with_conn(|c| {
            let total: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE source = 'inferred'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(total, 2, "no duplicate inferred edges after re-harvest");
            Ok(())
        })
        .unwrap();
    }

    // FTB Library's bytecode references JEI from its @JeiPlugin -- an optional
    // integration, not a hard dependency. A purely-inferred (bytecode) hard
    // reference to an item viewer must land optional, so swapping JEI for REI does
    // not report jei as a missing dependency.
    #[test]
    fn write_scan_downgrades_an_inferred_integration_host_to_optional() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![
                dseed("sha_jei", "jei", &["mezz/jei"], &[], &[], None),
                dseed(
                    "sha_ftb",
                    "ftblibrary",
                    &["dev/ftb/mods/ftblibrary"],
                    &["mezz/jei"], // hard bytecode ref to an item viewer
                    &[],
                    None,
                ),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(
            rep.inferred_requires, 0,
            "an inferred item-viewer reference is not a hard requirement"
        );
        assert_eq!(
            rep.inferred_optional, 1,
            "it lands as an optional integration"
        );
        r.with_conn(|c| {
            let ftb = queries::mod_id_for_alias(c, "modid", "ftblibrary")?.unwrap();
            let (target, kind): (String, String) = c.query_row(
                "SELECT target_modid, kind FROM relation WHERE from_mod_id = ?1",
                [ftb],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!((target.as_str(), kind.as_str()), ("jei", "optional_dep"));
            Ok(())
        })
        .unwrap();
    }

    // A prefix owned by two mods is an ambiguous shaded library: it resolves to no
    // single owner, so it yields no edge.
    #[test]
    fn write_scan_skips_ambiguous_shaded_prefix() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![
                dseed(
                    "sha_a",
                    "moda",
                    &["org/shaded", "moda/core"],
                    &[],
                    &[],
                    None,
                ),
                dseed(
                    "sha_b",
                    "modb",
                    &["org/shaded", "modb/core"],
                    &[],
                    &[],
                    None,
                ),
                dseed("sha_c", "modc", &["modc/core"], &["org/shaded"], &[], None),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.inferred_requires, 0, "ambiguous prefix -> no edge");
    }

    // Modrinth's version.dependencies are authoritative: they become typed edges
    // in the `modrinth:<project_id>` namespace, embedded/self deps are dropped,
    // and the bytecode inference is suppressed for the Modrinth mod.
    #[test]
    fn write_scan_takes_modrinth_deps_and_suppresses_bytecode() {
        let r = Registry::open_in_memory().unwrap();
        let mut moda = mseed(
            "sha_a",
            "moda",
            "PROJ_A",
            &[
                ("PROJ_LIB", "required", Some("VER123")), // pinned to an exact version
                ("PROJ_OPT", "optional", None),
                ("PROJ_BAD", "incompatible", None),
                ("PROJ_EMB", "embedded", None), // bundled -> no edge
                ("PROJ_A", "required", None),   // self -> skipped
            ],
        );
        // a real bytecode reference AND a jar declaration that would each be an edge
        // if not suppressed -- Modrinth's version.dependencies is the sole source
        moda.hard_refs = vec!["appeng/api".into()];
        moda.declared_deps = vec![("suppressed".into(), RelKind::Requires, None, None)];
        let scan = ScanData {
            jars: vec![
                moda,
                dseed("sha_ae2", "ae2", &["appeng/api"], &[], &[], None),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };

        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.modrinth_deps, 3, "required + optional + incompatible");
        assert_eq!(
            rep.inferred_requires, 0,
            "bytecode suppressed for a Modrinth mod"
        );
        assert_eq!(
            rep.declared_deps, 0,
            "jar declaration suppressed for a Modrinth mod"
        );

        r.with_conn(|c| {
            let moda_id = queries::mod_id_for_alias(c, "modid", "moda")?.unwrap();
            let mut stmt = c.prepare(
                "SELECT target_modid, kind, source, target_version_range FROM relation
                 WHERE from_mod_id = ?1 ORDER BY target_modid",
            )?;
            let rows = stmt
                .query_map([moda_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                rows,
                vec![
                    (
                        "modrinth:PROJ_BAD".into(),
                        "conflicts".into(),
                        "modrinth".into(),
                        None,
                    ),
                    // required dep pinned to its exact Modrinth version_id
                    (
                        "modrinth:PROJ_LIB".into(),
                        "requires".into(),
                        "modrinth".into(),
                        Some("VER123".into()),
                    ),
                    (
                        "modrinth:PROJ_OPT".into(),
                        "optional_dep".into(),
                        "modrinth".into(),
                        None,
                    ),
                ],
                "typed Modrinth edges with a version pin; embedded + self dropped; no inferred edge"
            );
            Ok(())
        })
        .unwrap();
    }

    // The hybrid case: a Modrinth mod's external dependency (no target project,
    // a bare file name) resolves against the mirror's own knowledge -- an
    // artifact by that exact filename, else the filename stem as a modid --
    // and an unresolvable one lands in the external: namespace so the resolver
    // can report it as out-of-ecosystem.
    #[test]
    fn write_scan_resolves_external_deps_through_the_mirror() {
        let r = Registry::open_in_memory().unwrap();
        let mut host = mseed("sha_host", "hostmod", "PROJ_H", &[]);
        host.modrinth_deps = vec![
            ModrinthDepSeed {
                project_id: None,
                dep_type: "required".into(),
                version_id: None,
                file_name: Some("CustomNPCs.jar".into()),
            },
            ModrinthDepSeed {
                project_id: None,
                dep_type: "required".into(),
                version_id: None,
                file_name: Some("Unknown-1.0.jar".into()),
            },
        ];
        let mut npc = jar("sha_npc", "customnpcs", Some("1"), vec!["forge".into()]);
        npc.filename = Some("CustomNPCs.jar".into());
        let scan = ScanData {
            jars: vec![host, npc],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let host_id = queries::mod_id_for_alias(c, "modid", "hostmod")?.unwrap();
            let mut stmt = c.prepare(
                "SELECT target_modid FROM relation WHERE from_mod_id = ?1 ORDER BY target_modid",
            )?;
            let targets = stmt
                .query_map([host_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                targets,
                vec![
                    "customnpcs".to_string(),
                    "external:Unknown-1.0.jar".to_string()
                ],
                "known filename bridges to the mod's selector; unknown stays external"
            );
            Ok(())
        })
        .unwrap();
    }

    // A self-hosted modern jar (no Modrinth): its declared metadata becomes typed,
    // version-ranged jar-meta edges.
    #[test]
    fn write_scan_takes_modern_declared_deps() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![ddseed(
                "sha_m",
                "mymod",
                &[
                    ("jei", RelKind::Requires, Some("[15,)")),
                    ("architectury", RelKind::OptionalDep, None),
                    ("badmod", RelKind::Conflicts, None),
                ],
            )],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.declared_deps, 3);

        r.with_conn(|c| {
            let mymod = queries::mod_id_for_alias(c, "modid", "mymod")?.unwrap();
            let mut stmt = c.prepare(
                "SELECT target_modid, kind, source, target_version_range FROM relation
                 WHERE from_mod_id = ?1 ORDER BY target_modid",
            )?;
            let rows = stmt
                .query_map([mymod], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(
                rows,
                vec![
                    (
                        "architectury".into(),
                        "optional_dep".into(),
                        "jar-meta".into(),
                        None,
                    ),
                    ("badmod".into(), "conflicts".into(), "jar-meta".into(), None),
                    (
                        "jei".into(),
                        "requires".into(),
                        "jar-meta".into(),
                        Some("[15,)".into()),
                    ),
                ],
                "typed declared edges with version ranges, sourced jar-meta"
            );
            Ok(())
        })
        .unwrap();
    }

    // A jar with modern-manifest declared deps is on the manifest tier of the
    // cascade: the loader enforces hard deps there, so the bytecode inference
    // is suppressed for it -- and when a dual-metadata jar's mcmod.info and
    // manifest disagree on a target's kind, the manifest edge outranks.
    #[test]
    fn manifest_deps_suppress_bytecode_and_outrank_mcmod() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = ddseed(
            "sha_m",
            "modernmod",
            &[("truelib", RelKind::Requires, None)],
        );
        a.hard_refs = vec!["appeng/api".into()]; // would be an edge if not suppressed
        a.requires = vec!["truelib".into()]; // mcmod spelling of the same dep
        let scan = ScanData {
            jars: vec![a, dseed("sha_ae2", "ae2", &["appeng/api"], &[], &[], None)],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(
            rep.inferred_requires, 0,
            "bytecode suppressed for a manifest-tier jar"
        );
        r.with_conn(|c| {
            let m = queries::mod_id_for_alias(c, "modid", "modernmod")?.unwrap();
            let confidences: Vec<i64> = {
                let mut stmt = c.prepare(
                    "SELECT confidence FROM relation WHERE from_mod_id = ?1 AND target_modid = 'truelib'
                     ORDER BY confidence DESC",
                )?;
                stmt.query_map([m], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            assert_eq!(
                confidences,
                vec![55],
                "the manifest edge occupies the deduped slot at its higher rank"
            );
            Ok(())
        })
        .unwrap();
    }

    // A Modrinth mod whose upstream version.dependencies is empty is NOT
    // authoritative: its jar declaration and bytecode still apply, so it does not
    // end up edgeless just because its Modrinth page is bare.
    #[test]
    fn empty_modrinth_deps_does_not_suppress_the_fallback() {
        let r = Registry::open_in_memory().unwrap();
        let mut moda = mseed("sha_a", "moda", "PROJ_A", &[]); // Modrinth, but no deps
        moda.hard_refs = vec!["appeng/api".into()];
        let scan = ScanData {
            jars: vec![
                moda,
                dseed("sha_ae2", "ae2", &["appeng/api"], &[], &[], None),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.modrinth_deps, 0);
        assert_eq!(
            rep.inferred_requires, 1,
            "bytecode fills in when Modrinth declared nothing"
        );
        r.with_conn(|c| {
            let moda_id = queries::mod_id_for_alias(c, "modid", "moda")?.unwrap();
            let target: String = c.query_row(
                "SELECT target_modid FROM relation WHERE from_mod_id = ?1 AND source = 'inferred'",
                [moda_id],
                |r| r.get(0),
            )?;
            assert_eq!(target, "ae2");
            Ok(())
        })
        .unwrap();
    }

    // The single-pass jar reader pulls every fact from one zip open: loader
    // marker, bytecode graph, mcmod.info, and modern metadata -- with
    // neoforge.mods.toml winning over a co-present mods.toml.
    #[test]
    fn read_jar_single_pass_extracts_all_facts() {
        use super::super::classfile::fixtures::{build_class, jar};
        let cls = build_class("mymod/Main", &["appeng/api/AEApi"], false, None);
        let bytes = jar(&[
            ("mymod/Main.class", &cls),
            ("mcmod.info", br#"[{"modid":"mymod","name":"MyMod"}]"#),
            (
                "META-INF/mods.toml",
                b"[[mods]]\nmodId=\"legacy\"\n[[dependencies.legacy]]\nmodId=\"legacydep\"\nmandatory=true",
            ),
            (
                "META-INF/neoforge.mods.toml",
                b"[[mods]]\nmodId=\"mymod\"\n[[dependencies.mymod]]\nmodId=\"jei\"\ntype=\"required\"",
            ),
        ]);
        let r = read_jar(&bytes);
        // the NeoForge marker file separates a NeoForge build from a Forge one
        assert_eq!(r.facts.loader.as_deref(), Some("neoforge"));
        assert!(r.bytecode.owned.contains("mymod"));
        assert!(r.bytecode.hard_refs.contains("appeng/api"));
        assert_eq!(r.mcmod.map(|i| i.modid), Some("mymod".to_string()));
        assert_eq!(
            r.modmeta.modid.as_deref(),
            Some("mymod"),
            "neoforge.mods.toml wins over mods.toml"
        );
        assert_eq!(
            r.modmeta
                .deps
                .iter()
                .map(|d| d.modid.as_str())
                .collect::<Vec<_>>(),
            vec!["jei"]
        );
    }

    // Chisel/HatStand class: a Forge mod shipping no mcmod.info or mods.toml, its
    // identity only in the class-level @Mod(modid=...). The reader must still name
    // it and mark it forge (the annotation is Forge-specific), so it is not skipped
    // as identity-less and stays visible on the mirror.
    #[test]
    fn read_jar_uses_mod_annotation_modid_and_marks_forge() {
        use super::super::classfile::fixtures::{build_class_modid, jar};
        let cls = build_class_modid("team/chisel/Chisel", "chisel");
        let bytes = jar(&[("team/chisel/Chisel.class", &cls)]);
        let r = read_jar(&bytes);
        assert_eq!(
            r.bytecode.mod_id.as_deref(),
            Some("chisel"),
            "modid comes from the @Mod annotation when no metadata file is present"
        );
        assert_eq!(
            r.facts.loader.as_deref(),
            Some("forge"),
            "an @Mod annotation implies forge even without a marker file"
        );
        assert!(r.mcmod.is_none(), "no mcmod.info in the jar");
    }

    // #1: two distinct jars of one mod with no version metadata both become
    // version='unknown'; the old UNIQUE(mod_id, version, target) crashed the
    // harvest on the second. sha1 is the only identity now.
    #[test]
    fn write_scan_allows_two_unversioned_jars_of_one_mod() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![
                jar("sha_x", "dup", None, vec!["forge".into()]),
                jar("sha_y", "dup", None, vec!["forge".into()]),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        let rep = r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        assert_eq!(rep.mods, 1, "same modid -> one identity");
        assert_eq!(rep.mod_versions, 2, "two distinct jars are two artifacts");
        r.with_conn(|c| {
            // both reachable as two versions of the one mod
            assert_eq!(queries::versions_of_mod(c, "modid", "dup")?.len(), 2);
            Ok(())
        })
        .unwrap();
    }

    // #2: a jar published for several loaders records every target; an empty
    // loader set falls back to 'any'; a re-harvest replaces the set, not appends.
    #[test]
    fn write_scan_records_target_set_with_any_fallback_and_replace() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![
                jar(
                    "sha_multi",
                    "multi",
                    Some("1"),
                    vec!["forge".into(), "fabric".into()],
                ),
                jar("sha_any", "tweak", Some("1"), vec![]),
            ],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let mut multi = queries::versions_of_mod(c, "modid", "multi")?[0]
                .targets
                .clone();
            multi.sort();
            assert_eq!(multi, vec!["fabric".to_string(), "forge".to_string()]);
            let tweak = queries::versions_of_mod(c, "modid", "tweak")?;
            assert_eq!(tweak[0].targets, vec!["any".to_string()], "empty -> any");
            Ok(())
        })
        .unwrap();

        // upstream dropped fabric support: the set shrinks, doesn't accumulate
        let scan2 = ScanData {
            jars: vec![jar("sha_multi", "multi", Some("1"), vec!["forge".into()])],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan2, "T1")).unwrap();
        r.with_conn(|c| {
            let multi = queries::versions_of_mod(c, "modid", "multi")?;
            assert_eq!(multi[0].targets, vec!["forge".to_string()], "set replaced");
            Ok(())
        })
        .unwrap();
    }

    // an authored artifact keeps its hand-set targets even when a re-harvest
    // reports a different loader set (the precious skip in set_mod_version_targets)
    #[test]
    fn write_scan_preserves_authored_targets() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![jar(
                "sha_p",
                "pmod",
                Some("1"),
                vec!["forge".into(), "fabric".into()],
            )],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_txn(|c| {
            c.execute(
                "UPDATE mod_version SET source = 'authored' WHERE sha1 = 'sha_p'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        // re-harvest now sees only forge upstream; the authored targets must hold
        let scan2 = ScanData {
            jars: vec![jar("sha_p", "pmod", Some("1"), vec!["forge".into()])],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan2, "T1")).unwrap();
        r.with_conn(|c| {
            let mut t = queries::versions_of_mod(c, "modid", "pmod")?[0]
                .targets
                .clone();
            t.sort();
            assert_eq!(
                t,
                vec!["fabric".to_string(), "forge".to_string()],
                "authored targets survive re-harvest"
            );
            Ok(())
        })
        .unwrap();
    }

    // The Modrinth environment flags land on the mods row through the project
    // alias, refresh when upstream changes them, survive a flagless re-scan,
    // reach a self-host-linked provider in the same run, and never touch an
    // authored row.
    #[test]
    fn write_scan_records_and_refreshes_env_flags() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = jar("sha_env", "envmod", Some("1"), vec!["forge".into()]);
        a.project_id = Some("PROJ_ENV".into());
        let scan_with = |envs: &[(&str, &str, &str)]| ScanData {
            jars: vec![a.clone()],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: envs
                .iter()
                .map(|(p, c, s)| (p.to_string(), (c.to_string(), s.to_string())))
                .collect(),
            modrinth_leg_ok: true,
        };
        let env = |r: &Registry| -> (Option<String>, Option<String>) {
            r.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT client_env, server_env FROM mods LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .unwrap()
        };

        let s0 = scan_with(&[("PROJ_ENV", "required", "unsupported")]);
        r.with_txn(|c| write_scan(c, &s0, "T0")).unwrap();
        assert_eq!(
            env(&r),
            (Some("required".into()), Some("unsupported".into()))
        );

        // upstream corrected the flags -> refreshed
        let s1 = scan_with(&[("PROJ_ENV", "optional", "optional")]);
        r.with_txn(|c| write_scan(c, &s1, "T1")).unwrap();
        assert_eq!(env(&r), (Some("optional".into()), Some("optional".into())));

        // a scan that could not reach Modrinth (no env objects) keeps the known ones
        let s2 = scan_with(&[]);
        r.with_txn(|c| write_scan(c, &s2, "T2")).unwrap();
        assert_eq!(env(&r), (Some("optional".into()), Some("optional".into())));

        // an authored mods row is precious: the flags do not move
        r.with_txn(|c| {
            c.execute("UPDATE mods SET source = 'authored'", [])?;
            Ok(())
        })
        .unwrap();
        let s3 = scan_with(&[("PROJ_ENV", "unsupported", "required")]);
        r.with_txn(|c| write_scan(c, &s3, "T3")).unwrap();
        assert_eq!(
            env(&r),
            (Some("optional".into()), Some("optional".into())),
            "authored row untouched"
        );
    }

    // A self-hosted provider that acquires its project alias through the slug
    // bridge gets that project's env flags in the same write_scan run.
    #[test]
    fn selfhost_link_applies_env_flags_in_the_same_run() {
        let r = Registry::open_in_memory().unwrap();
        let scan = ScanData {
            jars: vec![jar(
                "sha_arl",
                "autoreglib",
                Some("1"),
                vec!["forge".into()],
            )],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: HashMap::from([("NvZ9ZhwE".to_string(), "autoreglib".to_string())]),
            project_envs: HashMap::from([(
                "NvZ9ZhwE".to_string(),
                ("required".to_string(), "required".to_string()),
            )]),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let (client, server): (Option<String>, Option<String>) = c.query_row(
                "SELECT client_env, server_env FROM mods LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(
                (client.as_deref(), server.as_deref()),
                (Some("required"), Some("required")),
                "the dep project's flags land on the linked self-hosted mod"
            );
            Ok(())
        })
        .unwrap();
    }

    // A Modrinth channel lands on the release, and the provisional 'unknown'
    // release upsert_mod_version created is pruned once the file moves.
    #[test]
    fn write_scan_applies_channel_and_prunes_empty_release() {
        let r = Registry::open_in_memory().unwrap();
        let mut a = jar("sha_beta", "betamod", Some("0.9"), vec!["forge".into()]);
        a.channel = Some("beta".into());
        let scan = ScanData {
            jars: vec![a],
            packs: vec![],
            modrinth_modids_learned: 0,
            dep_project_slugs: Default::default(),
            project_envs: Default::default(),
            modrinth_leg_ok: true,
        };
        r.with_txn(|c| write_scan(c, &scan, "T0")).unwrap();
        r.with_conn(|c| {
            let mid = queries::mod_id_for_alias(c, "modid", "betamod")?.unwrap();
            let rels = queries::releases_of_mod_by_id(c, mid)?;
            assert_eq!(rels.len(), 1);
            assert_eq!(rels[0].channel, "beta");
            let total: i64 = c.query_row("SELECT count(*) FROM mod_release", [], |r| r.get(0))?;
            assert_eq!(total, 1, "no empty leftover release");
            Ok(())
        })
        .unwrap();
    }
}
