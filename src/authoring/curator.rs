//! Jar metadata extraction + the build enrichment passes that mutate a
//! [`PackConfig`] in place. Each pass is a separate function so `smrt-pack`
//! can run them from its own subcommand and inspect the result between steps
//! -- e.g. fill name/description from mcmod.info, then infer requires.
//!
//! All passes are idempotent: re-running with the same inputs yields the same
//! output. Passes that fill optional fields prefer existing data over derived
//! data, so a manual override always wins against a heuristic source.

use super::archive::read_zip_entry;
use super::sources::cache_jar_path;
use crate::domain::PackConfig;
use crate::domain::SourceDecl;
use crate::domain::{Display, Requirement};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;
use tracing::{debug, info, warn};
use ts_rs::TS;

// ── mcmod.info ────────────────────────────────────────────────────────────

/// Subset of the 1.12.2-era Forge `mcmod.info` schema the build
/// pipeline reads. Real-world files are mostly the array form
/// `[{...mod...}, ...]`; some older mods wrap a single object in
/// `{"modListVersion": 2, "modList": [{...}]}`. [`read_mcmod_info`]
/// flattens both into `Vec<McModInfo>`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct McModInfo {
    #[serde(default)]
    pub modid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// Forge's declared target MC version. Often absent or an unresolved gradle
    /// token (`${mcversion}`); harvest uses it only when it looks like a real
    /// version, as a network-free mc source for non-Modrinth jars.
    #[serde(default)]
    pub mcversion: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Hard requirements (`requiredMods`): mods that must be present or the game
    /// crashes. When populated it is authoritative -- a modid in `dependencies` but
    /// not here is a load-order hint, not a hard dep (WorldEditCUI lists `worldedit`
    /// in `dependencies` but requires only forge). When empty the author did not
    /// distinguish, so `dependencies` is the best hard-dep signal there is.
    #[serde(default, rename = "requiredMods")]
    pub required_mods: Vec<String>,
    /// 1.12-era Forge spells the author list `authorList`. Harvest reads it as a
    /// local, network-free author source (falling back to Modrinth only when the
    /// jar carries none).
    #[serde(default, rename = "authorList")]
    pub authors: Vec<String>,
    /// Path inside the jar to the mod's logo image (Forge `logoFile`), used to
    /// surface the mod's own icon in the panel.
    #[serde(default, rename = "logoFile")]
    pub logo_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
struct McModInfoListWrap {
    #[serde(rename = "modList")]
    mod_list: Vec<McModInfo>,
}

/// Reads `mcmod.info` from a jar's bytes. Returns the first entry if
/// any, since 1.12 mods overwhelmingly declare one modid per jar.
/// Returns `Ok(None)` for: jar without mcmod.info, malformed mcmod.info,
/// or empty mod list. Errors only on I/O / zip-corruption.
pub fn read_mcmod_info(jar_bytes: &[u8]) -> Result<Option<McModInfo>> {
    let mut zip = match zip::ZipArchive::new(Cursor::new(jar_bytes)) {
        Ok(z) => z,
        Err(e) => {
            debug!("jar is not a valid zip: {}", e);
            return Ok(None);
        }
    };
    let mut entry = match zip.by_name("mcmod.info") {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let size = entry.size();
    let raw = read_zip_entry(&mut entry, size, "mcmod.info")?;
    Ok(parse_mcmod_info(&raw))
}

/// Decode + parse every entry of an `mcmod.info`. A single 1.12 jar can declare
/// several mods (ForgeMultipart ships forgemultipartcbe + minecraftmultipartcbe +
/// microblockcbe; ReplayMod ships its submodules), and the array lists them all.
fn parse_mcmod_entries(raw: &[u8]) -> Vec<McModInfo> {
    // mcmod.info comes from many authors over many years. Lossy UTF-8 decode
    // handles the occasional ISO-8859-1 file. BOM strip handles the occasional
    // UTF-8-BOM-prefixed file from Windows authors.
    let decoded = String::from_utf8_lossy(raw);
    let trimmed = decoded.trim_start_matches('\u{FEFF}').trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Hand-authored mcmod.info files routinely embed raw newlines/tabs inside a
    // string value (a multi-line `description`, a `credits` block) -- invalid per
    // strict JSON, which serde_json enforces, so the whole file fails to parse and
    // an otherwise-identifiable mod (IronChest, and others) falls through to no
    // identity. Forge's own reader tolerates it. Neutralize control characters to
    // spaces first: inside a string this repairs the illegal literal; between
    // tokens a control char is insignificant whitespace, so a space is equivalent.
    let src: String = trimmed
        .chars()
        .map(|c| if (c as u32) < 0x20 { ' ' } else { c })
        .collect();
    // Two valid shapes per the Forge spec era:
    //   1. JSON array of mod entries
    //   2. JSON object with a `modList` field containing the array
    if src.starts_with('[') {
        serde_json::from_str::<Vec<McModInfo>>(&src).unwrap_or_default()
    } else {
        serde_json::from_str::<McModInfoListWrap>(&src)
            .map(|w| w.mod_list)
            .unwrap_or_default()
    }
}

/// The first mod an `mcmod.info` declares -- the jar's primary identity (the
/// zip-free core of [`read_mcmod_info`], so a single-pass jar reader can hand over
/// the entry it already extracted).
pub fn parse_mcmod_info(raw: &[u8]) -> Option<McModInfo> {
    parse_mcmod_entries(raw).into_iter().next()
}

/// Every non-empty modid an `mcmod.info` declares. For a jar bundling several mods,
/// each is a mod the jar provides -- registering them all lets a dependency on a
/// bundled modid resolve to the jar that ships it.
pub fn mcmod_modids(raw: &[u8]) -> Vec<String> {
    parse_mcmod_entries(raw)
        .into_iter()
        .map(|e| e.modid)
        .filter(|m| !m.is_empty())
        .collect()
}

/// Version-bearing metadata files excluded from the content signature: bumping
/// A loader hint harvest reads straight from a jar, for artifacts Modrinth can't
/// answer for.
#[derive(Debug, Clone, Default)]
pub struct JarFacts {
    /// `forge` (mcmod.info / mods.toml) or `fabric` (fabric.mod.json), else None.
    pub loader: Option<String>,
}

/// Introspect a jar for its loader marker (which mod-metadata file it carries).
/// A non-zip or marker-less jar yields None. Never errors -- best-effort.
pub fn jar_facts(jar_bytes: &[u8]) -> JarFacts {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(jar_bytes)) else {
        return JarFacts::default();
    };
    let mut has_forge = false;
    let mut has_fabric = false;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        if !entry.is_file() {
            continue;
        }
        match entry.name() {
            "mcmod.info" | "META-INF/mods.toml" => has_forge = true,
            "fabric.mod.json" => has_fabric = true,
            _ => {}
        }
    }
    let loader = if has_forge {
        Some("forge".to_string())
    } else if has_fabric {
        Some("fabric".to_string())
    } else {
        None
    };
    JarFacts { loader }
}

/// A plausible real Minecraft version, filtering out empty and unresolved gradle
/// tokens (`${mcversion}`) that mcmod.info files routinely carry.
pub fn clean_mc_version(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() || v.contains('$') || v.contains('{') {
        return None;
    }
    // a real MC version starts with a digit (1.7.10, 1.12.2, 1.20.1)
    if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Some(v.to_string())
    } else {
        None
    }
}

/// Extract a mod's embedded icon from its jar bytes: the Forge `mcmod.info`
/// `logoFile`, else the modern declared icon (`mods.toml` /
/// `neoforge.mods.toml` `logoFile`, `fabric.mod.json` `icon`), else a
/// conventional `pack.png` / `icon.png` / `logo.png` at the jar root. Returns
/// the image bytes plus a content-type guessed from the entry name. `Ok(None)`
/// when the jar has no recognizable icon or isn't a readable zip.
pub fn jar_icon(jar_bytes: &[u8]) -> Result<Option<(Vec<u8>, &'static str)>> {
    let mut zip = match zip::ZipArchive::new(Cursor::new(jar_bytes)) {
        Ok(z) => z,
        Err(_) => return Ok(None),
    };

    let mut candidates: Vec<String> = Vec::new();
    if let Ok(Some(info)) = read_mcmod_info(jar_bytes) {
        let lf = info.logo_file.trim().trim_start_matches('/');
        if !lf.is_empty() {
            candidates.push(lf.to_string());
        }
    }
    if let Some(logo) = super::modmeta::read_mod_meta(jar_bytes).logo_file {
        candidates.push(logo);
    }
    for d in ["pack.png", "icon.png", "logo.png"] {
        candidates.push(d.to_string());
    }

    for name in candidates {
        let read = match zip.by_name(&name) {
            Ok(mut e) if e.is_file() => {
                let size = e.size();
                Some(read_zip_entry(&mut e, size, &name)?)
            }
            _ => None,
        };
        if let Some(bytes) = read
            && !bytes.is_empty()
        {
            return Ok(Some((bytes, content_type_for(&name))));
        }
    }
    Ok(None)
}

fn content_type_for(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".jpg") || n.ends_with(".jpeg") {
        "image/jpeg"
    } else if n.ends_with(".gif") {
        "image/gif"
    } else {
        "image/png"
    }
}

// ── mcmod.info dependency lists ───────────────────────────────────────────

// mcmod.info dependency lists routinely name the platform, not a real mod.
// Compared lowercased, so the loader is dropped however a jar spells it.
const PSEUDO_DEPS: &[&str] = &[
    "forge",
    "minecraftforge",
    "mod_minecraftforge",
    "forgemodloader",
    "fml",
    "cpw.mods.fml",
    "mcp",
    "minecraft",
    "mod_mcversion",
    "neoforge",
    "fabric",
    "fabricloader",
    "cleanroom",
    "quilt",
];

/// Split and clean a jar's declared dependency list into plausible modids. Real
/// mcmod.info files vary wildly: a Forge dependency string
/// (`required-after:jei@[4.16,)`), a comma- or semicolon-joined list kept in one
/// entry (`forge,codechickenlib,cofhcore`), a human-readable phrase
/// (`ic2 experimental or ic2 classic`), or the platform itself. Split on the
/// separators, drop the Forge ordering prefix and the version window, drop the
/// platform, and keep only what reads as a modid -- so a bogus token never becomes
/// a relation the resolver then reports missing (#10). Order-preserving, deduped.
/// The hard-dependency modids a jar's `mcmod.info` declares. `requiredMods` is the
/// hard-require list; when the author filled it, it is authoritative and a modid
/// only in `dependencies` is a load-order hint, not a hard dep (WorldEditCUI
/// requires only forge, listing `worldedit` in `dependencies` alone). When
/// `requiredMods` is empty the author did not distinguish, so `dependencies` is the
/// best hard-dep signal there is. Cleaned through [`filter_deps`] either way.
pub fn mcmod_hard_deps(info: &McModInfo) -> Vec<String> {
    let hard = if info.required_mods.is_empty() {
        &info.dependencies
    } else {
        &info.required_mods
    };
    filter_deps(hard)
}

pub fn filter_deps(deps: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in deps {
        for token in raw.split([',', ';']) {
            let Some(modid) = clean_dep_token(token) else {
                continue;
            };
            if PSEUDO_DEPS.contains(&modid.to_lowercase().as_str()) {
                continue;
            }
            if seen.insert(modid.clone()) {
                out.push(modid);
            }
        }
    }
    out
}

/// One dependency token -> its bare modid, or None when it is not one. Drops a
/// Forge ordering prefix (`required-after:`, `after:`, ...) and the `@[range]`
/// window, then keeps the token only if what remains reads as a modid
/// (`[A-Za-z0-9_.-]+`) -- a phrase with spaces is a human-readable note, not a
/// modid, and cannot be resolved, so it is dropped rather than stored as junk.
fn clean_dep_token(token: &str) -> Option<String> {
    // a Forge dependency string is `<ordering>:<modid>`; the modid is the last
    // colon-segment (a real modid has no colons)
    let t = token.trim().rsplit(':').next().unwrap_or("").trim();
    // drop the version window
    let t = t.split('@').next().unwrap_or("").trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return None;
    }
    Some(t.to_string())
}

// ── Pass 1: enrich display from mcmod.info ────────────────────────────────

#[derive(Debug, Default)]
pub struct McModEnrichReport {
    pub mods_with_info: u32,
    pub mods_filled: u32,
    pub mods_skipped_modrinth: u32,
    pub mods_skipped_no_info: u32,
    pub mods_skipped_already_complete: u32,
}

/// Fills `display.name / description / url` on every smrt_cache-sourced
/// mod whose jar has a parseable `mcmod.info`. Existing values win --
/// this pass never overwrites a field the human already filled in.
///
/// Modrinth-sourced mods are skipped here; their display metadata
/// comes from the Modrinth project API and lands via a separate pass
/// (not yet implemented in this module).
pub fn enrich_from_mcmod_info(
    config: &mut PackConfig,
    storage: &Path,
) -> Result<McModEnrichReport> {
    let mut report = McModEnrichReport::default();
    for m in config.mods.iter_mut() {
        let sha1 = match &m.source {
            SourceDecl::SmrtCache { sha1 } => sha1.clone(),
            SourceDecl::Modrinth { .. } => {
                report.mods_skipped_modrinth += 1;
                continue;
            }
            SourceDecl::SmrtStatic { .. } => continue,
        };

        let display = m.display.get_or_insert_with(default_display);
        if display.name.is_some() && display.description.is_some() && display.url.is_some() {
            report.mods_skipped_already_complete += 1;
            continue;
        }

        let jar_path = cache_jar_path(storage, &sha1)?;
        let bytes = match fs::read(&jar_path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "cache jar {} not readable for {}: {}",
                    jar_path.display(),
                    m.filename,
                    e
                );
                continue;
            }
        };
        let info = match read_mcmod_info(&bytes)? {
            Some(i) => i,
            None => {
                report.mods_skipped_no_info += 1;
                continue;
            }
        };
        report.mods_with_info += 1;

        let mut filled_anything = false;
        if display.name.is_none() && !info.name.trim().is_empty() {
            display.name = Some(info.name.trim().to_string());
            filled_anything = true;
        }
        if display.description.is_none() && !info.description.trim().is_empty() {
            display.description = Some(info.description.trim().to_string());
            filled_anything = true;
        }
        if display.url.is_none() && !info.url.trim().is_empty() {
            display.url = Some(info.url.trim().to_string());
            filled_anything = true;
        }
        if filled_anything {
            report.mods_filled += 1;
        }
    }
    info!(
        with_info = report.mods_with_info,
        filled = report.mods_filled,
        skipped_mr = report.mods_skipped_modrinth,
        skipped_noinf = report.mods_skipped_no_info,
        skipped_full = report.mods_skipped_already_complete,
        "enrich-from-mcmod-info complete"
    );
    Ok(report)
}

// ── Pass 3: infer requires from mcmod.info dependencies ───────────────────

#[derive(Debug, Default)]
pub struct InferRequiresReport {
    pub mods_with_deps: u32,
    pub edges_added: u32,
    pub edges_skipped_unresolved: Vec<(String, String)>,
}

/// Walks every smrt_cache-sourced mod's declared hard dependencies and emits
/// `display.requires` entries pointing at sibling mods in the same pack. Modid
/// -> filename resolution uses the modid declared inside each jar's own
/// mcmod.info, so this only works for jars that declare one. Modrinth-sourced
/// mods are skipped (their deps live in the Modrinth API and need a separate
/// pass).
///
/// Hard means [`mcmod_hard_deps`], the same rule the harvest applies: a modid
/// that appears only in `dependencies` while `requiredMods` is filled is a
/// load-order hint, not a requirement. Reading the raw `dependencies` list here
/// -- which is what this pass used to do -- turned those hints into hard edges
/// at build time, and `derive_required` then locked their targets required,
/// so the same jar came out of a harvest and out of a build meaning different
/// things. It also drops the platform pseudo-deps and Forge's `@[range]`
/// syntax, neither of which resolves to a sibling mod.
///
/// Existing `display.requires` wins -- this pass never replaces an
/// authored list, only fills an empty one.
pub fn infer_requires_from_mcmod_info(
    config: &mut PackConfig,
    storage: &Path,
) -> Result<InferRequiresReport> {
    // First pass: build modid -> filename map across the pack.
    let mut modid_to_filename: HashMap<String, String> = HashMap::new();
    for m in &config.mods {
        let sha1 = match &m.source {
            SourceDecl::SmrtCache { sha1 } => sha1.clone(),
            _ => continue,
        };
        let jar_path = cache_jar_path(storage, &sha1)?;
        let Ok(bytes) = fs::read(&jar_path) else {
            continue;
        };
        let Some(info) = read_mcmod_info(&bytes)? else {
            continue;
        };
        if info.modid.is_empty() {
            continue;
        }
        // First-write wins so a duplicate modid (e.g. shadowed jar) is
        // logged but does not silently overwrite the canonical mapping.
        if let Some(existing) = modid_to_filename.get(&info.modid) {
            warn!(
                "duplicate modid {} mapped to both {} and {}; keeping the first",
                info.modid, existing, m.filename
            );
            continue;
        }
        modid_to_filename.insert(info.modid.clone(), m.filename.clone());
    }
    debug!(
        modids = modid_to_filename.len(),
        "built modid->filename map"
    );

    // Second pass: emit display.requires for each mod whose mcmod.info
    // declares dependencies that resolve against the map.
    let mut report = InferRequiresReport::default();
    let modids = modid_to_filename.clone();
    for m in config.mods.iter_mut() {
        let sha1 = match &m.source {
            SourceDecl::SmrtCache { sha1 } => sha1.clone(),
            _ => continue,
        };
        if let Some(d) = &m.display
            && !d.requires.is_empty()
        {
            continue;
        }
        let jar_path = cache_jar_path(storage, &sha1)?;
        let Ok(bytes) = fs::read(&jar_path) else {
            continue;
        };
        let Some(info) = read_mcmod_info(&bytes)? else {
            continue;
        };
        let hard = mcmod_hard_deps(&info);
        if hard.is_empty() {
            continue;
        }
        report.mods_with_deps += 1;

        let mut edges = Vec::new();
        for dep_modid in &hard {
            match modids.get(dep_modid) {
                Some(target_fname) => edges.push(Requirement {
                    filename: target_fname.clone(),
                    version_range: None,
                    optional: false,
                }),
                None => report
                    .edges_skipped_unresolved
                    .push((m.filename.clone(), dep_modid.clone())),
            }
        }
        if !edges.is_empty() {
            report.edges_added += edges.len() as u32;
            let display = m.display.get_or_insert_with(default_display);
            display.requires = edges;
        }
    }
    info!(
        with_deps = report.mods_with_deps,
        edges = report.edges_added,
        unresolved = report.edges_skipped_unresolved.len(),
        "infer-requires-from-mcmod-info complete"
    );
    Ok(report)
}

// ── helpers ───────────────────────────────────────────────────────────────

fn default_display() -> Display {
    Display::default()
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LoaderSpec;
    use crate::domain::{DeclaredMod, PackConfig, PackTier, SourceDecl, Visibility};
    use std::io::Write;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

    fn empty_config() -> PackConfig {
        PackConfig {
            pack_id: "Test".into(),
            display_name: "Test".into(),
            tagline: String::new(),
            minecraft_version: "1.12.2".into(),
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2922".into(),
            },
            java_major: 8,
            version: None,
            auth: None,
            tags: Vec::new(),
            featured: false,
            mods: Vec::new(),
            assets: Vec::new(),
            pack_meta: Default::default(),
            owner: 211033194,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    fn write_test_jar(dir: &Path, sha1: &str, mcmod_json: &str) -> Result<()> {
        // Place the fixture where the code reads it -- via the same layout helper.
        let jar_path = cache_jar_path(dir, sha1)?;
        if let Some(parent) = jar_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let f = fs::File::create(&jar_path)?;
        let mut zw = zip::ZipWriter::new(f);
        zw.start_file("mcmod.info", SimpleFileOptions::default())?;
        zw.write_all(mcmod_json.as_bytes())?;
        zw.finish()?;
        Ok(())
    }

    #[test]
    fn mcmod_hard_deps_trusts_required_mods_when_present() {
        // WorldEditCUI hard-requires only forge; worldedit is a load-order hint in
        // `dependencies`, so it must not become a hard dependency (a false missing).
        let cui = parse_mcmod_info(
            br#"[{"modid":"worldeditcuife2","requiredMods":["forge@[14,)"],"dependencies":["forge@[14,)","worldedit"]}]"#,
        )
        .unwrap();
        assert_eq!(
            mcmod_hard_deps(&cui),
            Vec::<String>::new(),
            "worldedit is load-order-only, not a hard dependency"
        );
        // GravitationSuite leaves requiredMods empty, so its `dependencies` ic2 is
        // the only hard-dep signal and stays hard.
        let gs = parse_mcmod_info(br#"[{"modid":"gravisuite","dependencies":["ic2"]}]"#).unwrap();
        assert_eq!(
            mcmod_hard_deps(&gs),
            vec!["ic2".to_string()],
            "no requiredMods -> dependencies is the hard signal"
        );
    }

    #[test]
    fn filter_deps_drops_platform_pseudo_deps() {
        let got = filter_deps(&[
            "appleskin".into(),
            "Forge".into(),
            "minecraft".into(),
            "  ".into(),
            "jei".into(),
        ]);
        assert_eq!(got, vec!["appleskin".to_string(), "jei".to_string()]);
    }

    #[test]
    fn filter_deps_splits_cleans_and_drops_junk() {
        let got = filter_deps(&[
            // comma-joined list, platform first -> loader dropped, rest split out
            "forge,codechickenlib,cofhcore,thermalfoundation".into(),
            // Forge dependency string: ordering prefix + version window stripped
            "required-after:jei@[4.16,)".into(),
            // the loader by another spelling, with a range
            "MinecraftForge@[14.21.0.2373,)".into(),
            "mod_MinecraftForge".into(),
            // human-readable phrases are not modids -> dropped; `chisel` survives
            "ic2 experimental or ic2 classic, chisel".into(),
            "Applied Energistics 2".into(),
            // duplicate collapses
            "codechickenlib".into(),
        ]);
        assert_eq!(
            got,
            vec![
                "codechickenlib".to_string(),
                "cofhcore".to_string(),
                "thermalfoundation".to_string(),
                "jei".to_string(),
                "chisel".to_string(),
            ]
        );
    }

    #[test]
    fn read_mcmod_info_handles_array_form() {
        // Standard form from 99% of 1.12.2 mods.
        let bytes = build_jar_bytes(
            r#"[{"modid":"appleskin","name":"AppleSkin","description":"Hunger viz","url":"https://modrinth.com/mod/appleskin","dependencies":["appleskin-api"]}]"#,
        );
        let info = read_mcmod_info(&bytes).unwrap().unwrap();
        assert_eq!(info.modid, "appleskin");
        assert_eq!(info.name, "AppleSkin");
        assert_eq!(info.dependencies, vec!["appleskin-api"]);
    }

    #[test]
    fn read_mcmod_info_handles_object_wrap_form() {
        // Older "modListVersion": 2 schema.
        let bytes = build_jar_bytes(
            r#"{"modListVersion":2,"modList":[{"modid":"oldmod","name":"OldMod"}]}"#,
        );
        let info = read_mcmod_info(&bytes).unwrap().unwrap();
        assert_eq!(info.modid, "oldmod");
    }

    #[test]
    fn parse_mcmod_info_tolerates_raw_newlines_in_strings() {
        // Hand-authored 1.12 files (IronChest and others) put a literal newline
        // inside a multi-line description -- invalid per strict JSON. The parse must
        // still recover the identity rather than dropping the whole file.
        let raw = "[{\"modid\":\"ironchest\",\"name\":\"Iron Chest\",\"description\":\"Bigger chests.\nCrystal chest is transparent.\"}]";
        let info = parse_mcmod_info(raw.as_bytes()).expect("parses despite raw newline");
        assert_eq!(info.modid, "ironchest");
        assert_eq!(info.name, "Iron Chest");
    }

    #[test]
    fn read_mcmod_info_returns_none_when_absent() {
        let bytes = build_jar_bytes_without_mcmod();
        let info = read_mcmod_info(&bytes).unwrap();
        assert!(info.is_none());
    }

    #[test]
    fn read_mcmod_info_tolerates_bom_and_whitespace() {
        let blob = "\u{FEFF}  [{\"modid\":\"bom_mod\"}]  ".to_string();
        let bytes = build_jar_bytes(&blob);
        let info = read_mcmod_info(&bytes).unwrap().unwrap();
        assert_eq!(info.modid, "bom_mod");
    }

    #[test]
    fn enrich_fills_only_missing_fields() {
        let dir = TempDir::new().unwrap();
        let sha = "a".repeat(40);
        write_test_jar(
            dir.path(),
            &sha,
            r#"[{"modid":"x","name":"FromJar","description":"FromJarDesc","url":"https://fromjar"}]"#,
        ).unwrap();

        let mut cfg = empty_config();
        cfg.mods.push(DeclaredMod {
            filename: "X.jar".into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha.clone() },
            display: Some(Display {
                name: Some("CuratorName".into()), // existing -- must win
                ..Display::default()
            }),
            slug: None,
            pulled: false,
        });

        let report = enrich_from_mcmod_info(&mut cfg, dir.path()).unwrap();
        assert_eq!(report.mods_with_info, 1);
        let d = cfg.mods[0].display.as_ref().unwrap();
        assert_eq!(d.name.as_deref(), Some("CuratorName"), "curator wins");
        assert_eq!(d.description.as_deref(), Some("FromJarDesc"));
        assert_eq!(d.url.as_deref(), Some("https://fromjar"));
    }

    #[test]
    fn infer_requires_resolves_modid_dependencies() {
        let dir = TempDir::new().unwrap();
        let sha_jei = "1".repeat(40);
        let sha_addon = "2".repeat(40);
        write_test_jar(dir.path(), &sha_jei, r#"[{"modid":"jei","name":"JEI"}]"#).unwrap();
        write_test_jar(
            dir.path(),
            &sha_addon,
            r#"[{"modid":"jeiaddon","name":"JEI Addon","dependencies":["jei"]}]"#,
        )
        .unwrap();

        let mut cfg = empty_config();
        cfg.mods.push(DeclaredMod {
            filename: "JEI.jar".into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha_jei },
            display: None,
            slug: None,
            pulled: false,
        });
        cfg.mods.push(DeclaredMod {
            filename: "JEIAddon.jar".into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha_addon },
            display: None,
            slug: None,
            pulled: false,
        });

        let report = infer_requires_from_mcmod_info(&mut cfg, dir.path()).unwrap();
        assert_eq!(report.edges_added, 1);
        let addon_deps = &cfg.mods[1].display.as_ref().unwrap().requires;
        assert_eq!(addon_deps.len(), 1);
        assert_eq!(addon_deps[0].filename, "JEI.jar");
    }

    #[test]
    fn infer_requires_reports_unresolved_modids() {
        let dir = TempDir::new().unwrap();
        let sha = "3".repeat(40);
        write_test_jar(
            dir.path(),
            &sha,
            r#"[{"modid":"lonely","dependencies":["ghost"]}]"#,
        )
        .unwrap();

        let mut cfg = empty_config();
        cfg.mods.push(DeclaredMod {
            filename: "Lonely.jar".into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha },
            display: None,
            slug: None,
            pulled: false,
        });

        let report = infer_requires_from_mcmod_info(&mut cfg, dir.path()).unwrap();
        assert_eq!(report.edges_added, 0);
        assert_eq!(
            report.edges_skipped_unresolved,
            vec![("Lonely.jar".into(), "ghost".into())]
        );
    }

    // The build reads the same hard-dependency rule the harvest does. A modid
    // that appears only in `dependencies` while `requiredMods` is filled is a
    // load-order hint (WorldEditCUI names `worldedit` there and requires only
    // forge); turning it into a `display.requires` edge is what makes
    // `derive_required` lock the target required, so the same jar would mean
    // one thing after a harvest and another after a build.
    #[test]
    fn infer_requires_reads_hard_dependencies_not_load_order_hints() {
        let dir = TempDir::new().unwrap();
        let sha_we = "4".repeat(40);
        let sha_cui = "5".repeat(40);
        write_test_jar(dir.path(), &sha_we, r#"[{"modid":"worldedit"}]"#).unwrap();
        write_test_jar(
            dir.path(),
            &sha_cui,
            r#"[{"modid":"worldeditcuife2","requiredMods":["forge@[14,)"],"dependencies":["forge@[14,)","worldedit"]}]"#,
        )
        .unwrap();

        let mut cfg = empty_config();
        for (filename, sha1) in [("WorldEdit.jar", sha_we), ("WorldEditCUI.jar", sha_cui)] {
            cfg.mods.push(DeclaredMod {
                filename: filename.into(),
                default_enabled: true,
                source: SourceDecl::SmrtCache { sha1 },
                display: None,
                slug: None,
                pulled: false,
            });
        }

        let report = infer_requires_from_mcmod_info(&mut cfg, dir.path()).unwrap();
        assert_eq!(
            report.edges_added, 0,
            "the platform is not a mod and worldedit is only a load-order hint"
        );
        assert!(
            cfg.mods[1]
                .display
                .as_ref()
                .is_none_or(|d| d.requires.is_empty())
        );
    }

    fn build_jar_bytes(mcmod_json: &str) -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        zw.start_file("mcmod.info", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(mcmod_json.as_bytes()).unwrap();
        zw.finish().unwrap().into_inner()
    }

    fn build_jar_bytes_without_mcmod() -> Vec<u8> {
        let buf = Cursor::new(Vec::new());
        let mut zw = zip::ZipWriter::new(buf);
        zw.start_file("META-INF/MANIFEST.MF", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"Manifest-Version: 1.0\n").unwrap();
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn jar_icon_prefers_logofile_then_pack_png_else_none() {
        // mcmod.info logoFile wins over a present pack.png
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zw.start_file("mcmod.info", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(br#"[{"modid":"x","logoFile":"assets/x/logo.png"}]"#)
            .unwrap();
        zw.start_file("assets/x/logo.png", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"LOGO").unwrap();
        zw.start_file("pack.png", SimpleFileOptions::default())
            .unwrap();
        zw.write_all(b"PACK").unwrap();
        let jar = zw.finish().unwrap().into_inner();
        let (bytes, ct) = jar_icon(&jar).unwrap().unwrap();
        assert_eq!(bytes, b"LOGO", "logoFile takes priority");
        assert_eq!(ct, "image/png");

        // a manifest-only jar yields no icon
        assert!(
            jar_icon(&build_jar_bytes_without_mcmod())
                .unwrap()
                .is_none()
        );

        // pack.png is the fallback when there's no logoFile
        let mut zw3 = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zw3.start_file("pack.png", SimpleFileOptions::default())
            .unwrap();
        zw3.write_all(b"P").unwrap();
        let jar3 = zw3.finish().unwrap().into_inner();
        assert_eq!(jar_icon(&jar3).unwrap().unwrap().0, b"P");
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zw.start_file(*name, SimpleFileOptions::default()).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn jar_facts_detects_loader_marker() {
        let fabric = zip_of(&[("A.class", b"X"), ("fabric.mod.json", b"{}")]);
        assert_eq!(jar_facts(&fabric).loader.as_deref(), Some("fabric"));
        let bare = zip_of(&[("A.class", b"X")]);
        assert_eq!(jar_facts(&bare).loader, None);
    }

    #[test]
    fn clean_mc_version_filters_tokens() {
        assert_eq!(clean_mc_version("1.12.2").as_deref(), Some("1.12.2"));
        assert_eq!(clean_mc_version(" 1.7.10 ").as_deref(), Some("1.7.10"));
        assert_eq!(clean_mc_version("${mcversion}"), None);
        assert_eq!(clean_mc_version(""), None);
        assert_eq!(clean_mc_version("MC1.7"), None);
    }
}
