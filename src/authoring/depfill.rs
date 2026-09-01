//! Dependency auto-fill, run when a pack config is saved. A curator adds the mods
//! they want; the mirror pulls in each mod's missing hard dependencies -- from
//! Modrinth first, else from its own cache -- so the operator never hand-manages
//! required libraries. It then records the resolved dependency graph in
//! `display.requires`, which is what the build reads to derive each mod's
//! required-ness (a dependency of a present mod is locked required; a top-level
//! mod stays optional unless its own classification requires it).

use super::modrinth::{Modrinth, Version as MrVersion};
use super::resolve;
use super::sources::ModrinthCache;
use crate::domain::{DeclaredMod, Display, PackConfig, Requirement, SourceDecl};
use crate::registry::{Registry, queries, semver};
use crate::storage::Storage;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A dep chain deeper than this is almost certainly a resolution loop, not a real
/// tree; stop pulling rather than spin.
const MAX_PASSES: usize = 8;

/// Pull each declared mod's missing hard dependencies in (Modrinth first, the
/// mirror's own cache second), then write the resolved requires graph into
/// `display.requires`. A dep's own dependencies come in on the next pass; the
/// loop stops once a pass adds nothing. A dependency neither source can provide
/// is left for the resolve report to flag, not invented. Only a jar whose bytes
/// the mirror actually holds can be declared as a `smrt_cache` source, which is
/// what `storage` is asked -- about the candidates that come up, rather than by
/// listing the whole cache before anything is known.
pub async fn fill_dependencies(
    cfg: &mut PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    storage: &Storage,
) -> Result<usize> {
    // Read once, reused by every pass: the wire pass below walks the same pins
    // each time round, and a pass that adds nothing must not cost a round trip.
    // The separate version cache serves the pinned-dependency leg of the resolve.
    let mut read: HashMap<String, MrVersion> = HashMap::new();
    let versions = ModrinthCache::default();
    let mut added_total = 0;
    for _ in 0..MAX_PASSES {
        let plan = plan_for(cfg, registry, modrinth, &mut read).await?;
        let mut added = false;
        for target in &plan.missing {
            // Per-target isolation: one unresolvable target (a Modrinth
            // outage, a dead project) must not abort the whole pass -- the
            // other targets still fill, and this one stays in the resolve
            // report's missing list instead of silently taking the rest
            // down with it.
            let decl =
                match resolve_target(target, cfg, registry, modrinth, storage, &versions).await {
                    Ok(Some(d)) => d,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(
                            target = %target.selector,
                            error = %e,
                            "dependency target unresolved this pass; skipped"
                        );
                        continue;
                    }
                };
            if !already_present(cfg, &decl) {
                cfg.mods.push(decl);
                added = true;
                added_total += 1;
            }
        }
        if !added {
            break;
        }
    }
    // record the final graph so the build derives required-ness from it
    let plan = plan_for(cfg, registry, modrinth, &mut read).await?;
    apply_requires(cfg, &plan.requires);
    prune_orphaned_pulled(cfg);
    Ok(added_total)
}

/// What this config still needs: the registry's own fill plan, folded together
/// with the dependencies Modrinth declares for pins the harvest has not read.
///
/// The registry half runs off the runtime, on a clone of the config. The plan
/// walks every declared mod against the relation graph, and this is called once
/// per fill pass on a path that runs about every second while somebody types in
/// the editor -- so it is the last place to hold a runtime worker on a mutex
/// the harvest also wants.
async fn plan_for(
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    read: &mut HashMap<String, MrVersion>,
) -> Result<resolve::DepFillPlan> {
    let snapshot = cfg.clone();
    let mut plan = registry
        .read(move |c| resolve::dependency_fill_plan(c, &snapshot))
        .await?;
    merge_wire_deps(&mut plan, cfg, registry, modrinth, read).await;
    Ok(plan)
}

/// One hard dependency read straight off a Modrinth version, before the mirror
/// has any registry knowledge of the pin that declares it.
struct WireDep {
    /// The declaring mod's filename -- the `from` side of the requires edge.
    requirer: String,
    project_id: String,
    /// The exact version the dependency names, when it names one.
    version_id: Option<String>,
}

/// Fold Modrinth's own dependency declarations into a registry fill plan.
///
/// The registry learns a Modrinth version only once the harvest has read it, and
/// the harvest reads the cache and published manifests -- so a mod just picked in
/// the panel, or an existing mod re-pinned to a newer build, contributes no edges
/// there at all, and its libraries would silently go unpulled until the pack had
/// been built and harvested once. The dependencies are right there in the version
/// JSON the resolver already fetches, so they are read from the wire for exactly
/// the pins the registry cannot speak for. A pin the harvest has read is left to
/// the registry, which holds the same facts at artifact granularity plus
/// everything the bytecode pass added.
///
/// A target the pack already ships becomes a requires edge (so the build locks
/// it); anything else becomes an auto-pull candidate. Per-pin isolation: an
/// unreachable upstream drops that pin's contribution and nothing else.
async fn merge_wire_deps(
    plan: &mut resolve::DepFillPlan,
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    read: &mut HashMap<String, MrVersion>,
) {
    let deps = wire_deps(cfg, registry, modrinth, read).await;
    let known: HashSet<&str> = plan.missing.iter().map(|t| t.selector.as_str()).collect();
    let mut extra: Vec<resolve::MissingTarget> = Vec::new();
    for d in &deps {
        match cfg.mods.iter().find(|m| {
            matches!(&m.source, SourceDecl::Modrinth { project_id, .. } if *project_id == d.project_id)
        }) {
            Some(present) => plan
                .requires
                .push((d.requirer.clone(), present.filename.clone())),
            None => {
                let selector = format!("modrinth:{}", d.project_id);
                if known.contains(selector.as_str())
                    || extra.iter().any(|t| t.selector == selector)
                {
                    continue;
                }
                extra.push(resolve::MissingTarget {
                    selector,
                    version_range: None,
                    pinned_version: d.version_id.clone(),
                });
            }
        }
    }
    plan.missing.extend(extra);
}

/// Read the required dependencies of every Modrinth pin the registry has not
/// harvested. One batched lookup per pass over the pins not read yet, so a pack
/// whose mods are all known costs no request at all and a fresh one costs a
/// single call rather than a round trip per mod. A failed lookup is non-fatal:
/// nothing is pulled from it and the resolve report still flags what is missing.
async fn wire_deps(
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    read: &mut HashMap<String, MrVersion>,
) -> Vec<WireDep> {
    let declared: Vec<(String, String, String)> = cfg
        .mods
        .iter()
        .filter_map(|m| match &m.source {
            SourceDecl::Modrinth {
                project_id,
                version_id,
            } => Some((m.filename.clone(), project_id.clone(), version_id.clone())),
            _ => None,
        })
        .collect();
    // Which pins the harvest has already read -- asked once for the whole set
    // rather than once per pin from inside an iterator chain running on the
    // runtime. A pin the harvest has read is the registry's to speak for.
    let harvested: HashSet<String> = {
        let ids: Vec<String> = declared.iter().map(|(_, _, v)| v.clone()).collect();
        registry
            .read(move |c| {
                let mut out = HashSet::new();
                for id in ids {
                    if queries::mod_version_id_for_modrinth_version_id(c, &id)?.is_some() {
                        out.insert(id);
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default()
    };
    let pins: Vec<(String, String, String)> = declared
        .into_iter()
        .filter(|(_, _, version_id)| !harvested.contains(version_id))
        .collect();
    let unread: Vec<String> = pins
        .iter()
        .map(|(_, _, version_id)| version_id.clone())
        .filter(|v| !read.contains_key(v))
        .collect();
    if !unread.is_empty() {
        match modrinth.versions_by_ids(&unread).await {
            Ok(found) => read.extend(found),
            Err(e) => tracing::warn!(
                pins = unread.len(),
                error = %e,
                "could not read pinned versions from Modrinth this pass; their dependencies are not pulled"
            ),
        }
    }
    let mut out = Vec::new();
    for (requirer, project_id, version_id) in pins {
        let Some(v) = read.get(&version_id) else {
            continue;
        };
        for dep in &v.dependencies {
            if dep.dependency_type != "required" {
                continue;
            }
            let Some(target) = dep.project_id.as_deref().filter(|p| !p.is_empty()) else {
                continue; // an external dependency names a file, not a project
            };
            if target == project_id {
                continue; // a project never depends on itself
            }
            out.push(WireDep {
                requirer: requirer.clone(),
                project_id: target.to_string(),
                version_id: dep.version_id.clone(),
            });
        }
    }
    out
}

/// Sticky-dependency merge, run by the save path BEFORE the fill: every pulled
/// entry the previously-saved config carries and the incoming body lacks is
/// carried over. A client that never saw a server-pulled dependency (a stale
/// editor, a scripted PUT of a hand-written list) must not delete it -- and an
/// upstream outage during the following fill must not either, because the
/// entry no longer depends on being re-resolvable that moment. Curator-declared
/// entries are never resurrected: removing one is an explicit act.
pub fn merge_pulled(saved: &PackConfig, incoming: &mut PackConfig) {
    let mut present: HashSet<String> = HashSet::new();
    for m in &incoming.mods {
        present.insert(source_identity(&m.source));
        present.insert(format!("f:{}", m.filename));
    }
    for m in saved.mods.iter().filter(|m| m.pulled) {
        // matched by source identity OR filename: either means the incoming
        // body already carries this dependency in some form
        if !present.contains(&source_identity(&m.source))
            && !present.contains(&format!("f:{}", m.filename))
        {
            incoming.mods.push(m.clone());
        }
    }
}

/// One dependency a save would pull in, answered before the save happens.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub struct PulledPreview {
    pub filename: String,
    /// Where the mirror would take it from: `modrinth` or the mirror's own cache.
    pub source: String,
    /// The declared mods that require it, by filename. A library two mods need
    /// names both, so removing one does not imply the library leaves.
    pub needed_by: Vec<String>,
}

/// What saving this config would pull in, computed without saving anything.
///
/// Adding a mod used to be blind: the dependencies arrived later, on save, and
/// whoever added it found out afterwards by opening the preview or resolving by
/// hand (#53). The plan was always computed -- it is what runs on save -- it was
/// simply never asked for in advance.
///
/// It runs the real fill on a copy rather than reimplementing the rules, so the
/// answer cannot drift from what the save will actually do; nothing is written.
pub async fn preview_fill(
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    storage: &Storage,
) -> Result<Vec<PulledPreview>> {
    let before: HashSet<String> = cfg
        .mods
        .iter()
        .map(|m| source_identity(&m.source))
        .collect();
    let mut filled = cfg.clone();
    fill_dependencies(&mut filled, registry, modrinth, storage).await?;

    // who requires what, among the mods the filled config would hold
    let plan = {
        let snapshot = filled.clone();
        registry
            .read(move |c| resolve::dependency_fill_plan(c, &snapshot))
            .await?
    };
    let mut needed_by: HashMap<&str, Vec<String>> = HashMap::new();
    for (requirer, dep) in &plan.requires {
        needed_by
            .entry(dep.as_str())
            .or_default()
            .push(requirer.clone());
    }

    Ok(filled
        .mods
        .iter()
        .filter(|m| !before.contains(&source_identity(&m.source)))
        .map(|m| {
            let mut who = needed_by
                .get(m.filename.as_str())
                .cloned()
                .unwrap_or_default();
            who.sort();
            who.dedup();
            PulledPreview {
                filename: m.filename.clone(),
                source: match &m.source {
                    SourceDecl::Modrinth { .. } => "modrinth".to_string(),
                    SourceDecl::SmrtCache { .. } => "cache".to_string(),
                    SourceDecl::SmrtStatic { .. } => "static".to_string(),
                },
                needed_by: who,
            }
        })
        .collect())
}

/// The identity a pulled entry is matched by across saves: the Modrinth
/// project (a re-pin to another version is still the same dependency), the
/// cache sha1, the static path.
fn source_identity(s: &SourceDecl) -> String {
    match s {
        SourceDecl::Modrinth { project_id, .. } => format!("m:{project_id}"),
        SourceDecl::SmrtCache { sha1 } => format!("c:{sha1}"),
        SourceDecl::SmrtStatic { rel_path } => format!("s:{rel_path}"),
    }
}

/// Drop pulled entries no curator-declared mod still reaches through hard
/// requires edges. Reachability, not per-edge presence, so a chain of pulled
/// libraries (A -> lib1 -> lib2) lives exactly as long as its curator root
/// does. Runs after `apply_requires`, which derives the edges from the
/// registry locally -- available in any upstream weather.
fn prune_orphaned_pulled(cfg: &mut PackConfig) {
    let hard_edges: HashMap<&str, Vec<&str>> = cfg
        .mods
        .iter()
        .map(|m| {
            let targets = m
                .display
                .as_ref()
                .map(|d| {
                    d.requires
                        .iter()
                        .filter(|r| !r.optional)
                        .map(|r| r.filename.as_str())
                        .collect()
                })
                .unwrap_or_default();
            (m.filename.as_str(), targets)
        })
        .collect();
    let mut reachable: HashSet<String> = HashSet::new();
    let mut queue: Vec<&str> = cfg
        .mods
        .iter()
        .filter(|m| !m.pulled)
        .map(|m| m.filename.as_str())
        .collect();
    while let Some(f) = queue.pop() {
        if reachable.insert(f.to_string())
            && let Some(targets) = hard_edges.get(f)
        {
            queue.extend(targets.iter().copied());
        }
    }
    let before = cfg.mods.len();
    cfg.mods
        .retain(|m| !m.pulled || reachable.contains(&m.filename));
    let dropped = before - cfg.mods.len();
    if dropped > 0 {
        tracing::info!(
            dropped,
            "pruned pulled dependencies nothing declares anymore"
        );
    }
}

/// Resolve a missing dependency to a declarable source, by priority: a Modrinth
/// version for the pack's Minecraft version and loader, else a cached artifact
/// of the mod the selector names (checked against the requirer's version window
/// where one is declared and comparable). `None` when neither source can
/// provide it -- the resolve report flags it instead.
async fn resolve_target(
    target: &resolve::MissingTarget,
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
    storage: &Storage,
    versions: &ModrinthCache,
) -> Result<Option<DeclaredMod>> {
    let bare = target
        .selector
        .split('@')
        .next()
        .unwrap_or(&target.selector);
    if bare.starts_with("external:") {
        return Ok(None); // outside both ecosystems by definition
    }
    let project = match bare.strip_prefix("modrinth:") {
        Some(p) => Some(p.to_string()),
        None => {
            let sel = bare.to_string();
            registry
                .read(move |c| {
                    Ok(queries::mod_id_for_selector(c, &sel)?
                        .and_then(|id| queries::modrinth_id_for_mod(c, id).ok().flatten()))
                })
                .await?
        }
    };
    if let Some(project) = project {
        let loader = cfg.loader.name.to_ascii_lowercase();
        // A dependency that names an exact version is honoured as pinned -- the
        // requirer asked for that build, not for whatever is newest. A pin that
        // does not suit the pack's loader, or that upstream published without a
        // jar, falls through to the newest usable build rather than declaring
        // something the mirror cannot fetch.
        if let Some(version_id) = &target.pinned_version {
            match versions.get_or_fetch(modrinth, &project, version_id).await {
                Ok(v) if usable(&v, &loader) => {
                    return Ok(Some(pulled_from_version(&project, &v)));
                }
                Ok(_) => tracing::warn!(
                    project = %project,
                    version = %version_id,
                    "pinned dependency version is unusable; resolving the newest compatible build"
                ),
                Err(e) => tracing::warn!(
                    project = %project,
                    version = %version_id,
                    error = %e,
                    "pinned dependency version unreadable; resolving the newest compatible build"
                ),
            }
        }
        let listing = modrinth
            .project_versions(&project, Some(&cfg.minecraft_version))
            .await?;
        // Modrinth returns versions newest-first, so the first usable one is the
        // latest compatible build.
        if let Some(v) = listing.into_iter().find(|v| usable(v, &loader)) {
            return Ok(Some(pulled_from_version(&project, &v)));
        }
    }
    // Modrinth cannot provide it: fall back to the mirror's own cache.
    resolve_from_cache(target, cfg, registry, storage).await
}

/// Whether a Modrinth version can actually be declared: it runs on the pack's
/// loader, and upstream published a file for it. A version with an empty `files`
/// array is a broken publish (the metadata landed, the jar did not) -- pinning it
/// would only fail at build time, so it is never chosen.
fn usable(v: &MrVersion, loader: &str) -> bool {
    v.loaders.iter().any(|l| l.eq_ignore_ascii_case(loader)) && v.primary_file().is_some()
}

/// A usable Modrinth version as a pulled declaration.
fn pulled_from_version(project: &str, v: &MrVersion) -> DeclaredMod {
    DeclaredMod {
        filename: v
            .primary_file()
            .map(|f| f.filename.clone())
            .unwrap_or_else(|| format!("{project}.jar")),
        default_enabled: true,
        source: SourceDecl::Modrinth {
            project_id: project.to_string(),
            version_id: v.id.clone(),
        },
        display: None,
        slug: None,
        pulled: true,
    }
}

/// The cache leg of the chain: the selector's mod, its artifacts narrowed to the
/// pack's loader family and Minecraft version, the requirer's version window
/// applied where comparable, and the newest survivor whose bytes the mirror
/// actually holds.
///
/// The registry decides which artifacts are eligible; the cache is then asked
/// about those, newest first, one `stat` at a time. It used to be handed the
/// mirror's whole cache listing instead -- built by the caller before any of
/// this ran, on every config save, so answering a question about a handful of
/// jars meant opening every directory in the cache.
async fn resolve_from_cache(
    target: &resolve::MissingTarget,
    cfg: &PackConfig,
    registry: &Arc<Registry>,
    storage: &Storage,
) -> Result<Option<DeclaredMod>> {
    let selector = target.selector.clone();
    let range = target.version_range.clone();
    let loader = cfg.loader.name.to_ascii_lowercase();
    let mc = cfg.minecraft_version.clone();
    let candidates: Vec<DeclaredMod> = registry
        .read(move |c| {
            let Some(mod_id) = queries::mod_id_for_selector(c, &selector)? else {
                return Ok(Vec::new());
            };
            let chain = queries::loader_chain(c, &loader)?;
            let mut out = Vec::new();
            for v in queries::versions_of_mod_by_id(c, mod_id)? {
                let loader_ok = v
                    .targets
                    .iter()
                    .any(|t| t == "any" || chain.contains(&t.to_lowercase()));
                let mc_ok = v.mc_versions.is_empty() || v.mc_versions.contains(&mc);
                if !loader_ok || !mc_ok {
                    continue;
                }
                // the requirer's window: reject a plainly out-of-window artifact;
                // an incomparable version passes (never act on a guess)
                if let Some(r) = range.as_deref()
                    && semver::in_range(&v.version, r) == Some(false)
                {
                    continue;
                }
                // Prefer the registry's stored filename. Absent (a cache jar ingested
                // without a harvest), name it after the requested modid rather than an
                // opaque `<sha1>.jar`: the selector on the cache leg is a bare modid
                // (a `modrinth:`/`external:` selector resolves on the Modrinth leg and
                // never reaches here), and a human name is what the panel and the
                // launcher's mods/<filename> both want. Sha1 stays the last resort.
                let filename = v.filename.clone().unwrap_or_else(|| {
                    if selector.contains(':') {
                        format!("{}.jar", v.sha1)
                    } else {
                        format!("{selector}.jar")
                    }
                });
                out.push(DeclaredMod {
                    filename,
                    default_enabled: true,
                    source: SourceDecl::SmrtCache {
                        sha1: v.sha1.clone(),
                    },
                    display: None,
                    slug: None,
                    pulled: true,
                });
            }
            Ok(out)
        })
        .await?;
    // rows come version-ordered, so the newest acceptable one is last
    for decl in candidates.into_iter().rev() {
        let SourceDecl::SmrtCache { sha1 } = &decl.source else {
            continue;
        };
        if storage.has_cache_jar(sha1).await {
            return Ok(Some(decl));
        }
    }
    Ok(None)
}

/// A pulled dependency is already in the pack when its source identity is
/// declared -- the Modrinth project id or the cache sha1, so a dep is not
/// re-added under a different display name -- or when its filename is taken,
/// since two rows writing one `mods/<filename>` is never a pack the build may
/// ship.
fn already_present(cfg: &PackConfig, decl: &DeclaredMod) -> bool {
    if cfg.mods.iter().any(|m| m.filename == decl.filename) {
        return true;
    }
    match &decl.source {
        SourceDecl::Modrinth { project_id, .. } => cfg.mods.iter().any(
            |m| matches!(&m.source, SourceDecl::Modrinth { project_id: p, .. } if p == project_id),
        ),
        SourceDecl::SmrtCache { sha1 } => cfg
            .mods
            .iter()
            .any(|m| matches!(&m.source, SourceDecl::SmrtCache { sha1: s } if s == sha1)),
        SourceDecl::SmrtStatic { .. } => false,
    }
}

/// Overwrite each mod's `display.requires` with the resolved present-mod edges, so
/// the field mirrors the registry graph exactly (no stale hand-entered deps linger).
fn apply_requires(cfg: &mut PackConfig, requires: &[(String, String)]) {
    let mut by_from: HashMap<&str, Vec<String>> = HashMap::new();
    for (from, dep) in requires {
        by_from.entry(from.as_str()).or_default().push(dep.clone());
    }
    for m in &mut cfg.mods {
        let mut deps = by_from
            .get(m.filename.as_str())
            .cloned()
            .unwrap_or_default();
        deps.sort();
        deps.dedup();
        if deps.is_empty() {
            // leave a hand-authored display untouched apart from clearing its stale
            // requires; do not materialize a display just to hold an empty list
            if let Some(d) = &mut m.display {
                d.requires.clear();
            }
            continue;
        }
        let reqs = deps
            .into_iter()
            .map(|filename| Requirement {
                filename,
                version_range: None,
                optional: false,
            })
            .collect();
        m.display.get_or_insert_with(Display::default).requires = reqs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LoaderSpec;
    use crate::domain::pack::{default_owner, default_tier, default_visibility};

    fn one(filename: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache {
                sha1: "0".repeat(40),
            },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    fn cfg(mods: Vec<DeclaredMod>) -> PackConfig {
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
            pack_meta: Default::default(),
            owner: default_owner(),
            tier: default_tier(),
            visibility: default_visibility(),
            fork_of: None,
        }
    }

    /// The hash of a test artifact, named by the tag its bytes are. Storage
    /// keys a jar by its own hash and verifies it on write, so a test hash has
    /// to be a real one -- the tag keeps the call site readable.
    fn sha(tag: &str) -> String {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(tag.as_bytes());
        hex::encode(h.finalize())
    }

    /// A cache that actually holds the named jars. The fill asks storage
    /// whether it has a candidate rather than being handed a set of hashes, so
    /// the tests hand it a cache with those jars in it.
    async fn cache_holding(tags: &[&str]) -> (tempfile::TempDir, Storage) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Storage::new(tmp.path().to_path_buf());
        for tag in tags {
            store
                .save_cache_jar(&sha(tag), tag.as_bytes())
                .await
                .unwrap();
        }
        (tmp, store)
    }

    /// A cache with nothing in it, for the cases where the fallback must find
    /// no candidate at all.
    fn empty_cache() -> (tempfile::TempDir, Storage) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Storage::new(tmp.path().to_path_buf());
        (tmp, store)
    }

    use crate::registry::model::{RelKind, Source};
    use crate::registry::{Registry, upsert};

    const NOW: &str = "2026-07-18T00:00:00Z";

    fn cache_mod(filename: &str, sha: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha.into() },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    fn add_artifact(r: &Registry, modid: &str, version: &str, sha: &str, filename: &str) -> i64 {
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", modid)], NOW)?;
            upsert::upsert_mod_version(
                c,
                id,
                version,
                &["forge"],
                sha,
                10,
                Some(filename),
                None,
                NOW,
            )?;
            Ok(id)
        })
        .unwrap()
    }

    // The same answer the save would give, before the save happens: what a mod
    // brings with it, who needs it, and nothing written to the config that was
    // asked about.
    #[tokio::test]
    async fn preview_reports_what_a_save_would_pull_without_touching_the_config() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "modb", "1.0", &sha("sha_b"), "modb-1.0.jar");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                a,
                None,
                "modb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_b"]).await;

        let preview = preview_fill(&c, &r, &modrinth, &store).await.unwrap();
        assert_eq!(preview.len(), 1, "one dependency would come with a.jar");
        assert_eq!(preview[0].filename, "modb-1.0.jar");
        assert_eq!(preview[0].source, "cache");
        assert_eq!(
            preview[0].needed_by,
            vec!["a.jar".to_string()],
            "the preview says which declared mod asks for it"
        );
        assert_eq!(
            c.mods.len(),
            1,
            "asking what a save would do must not do it"
        );

        // and it agrees with the save: filling for real adds exactly that row
        fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        let filled: Vec<&str> = c
            .mods
            .iter()
            .filter(|m| m.pulled)
            .map(|m| m.filename.as_str())
            .collect();
        assert_eq!(filled, vec!["modb-1.0.jar"]);
    }

    // The cache leg of the source chain: a hard dependency Modrinth cannot
    // provide is pulled from the mirror's own cache, once, and a re-run adds
    // nothing (deduped by sha1).
    #[tokio::test]
    async fn cache_fallback_pulls_a_cached_artifact() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "modb", "1.0", &sha("sha_b"), "modb-1.0.jar");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                a,
                None,
                "modb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_b"]).await;

        let added = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert_eq!(added, 1, "the cached dependency is pulled");
        let pulled = c
            .mods
            .iter()
            .find(|m| m.filename == "modb-1.0.jar")
            .unwrap();
        assert!(
            matches!(&pulled.source, SourceDecl::SmrtCache { sha1 } if sha1 == &sha("sha_b")),
            "declared as a smrt_cache source"
        );
        // the requires edge landed so the build locks the pulled dep
        let reqs = &c.mods[0].display.as_ref().unwrap().requires;
        assert_eq!(reqs[0].filename, "modb-1.0.jar");

        let again = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert_eq!(again, 0, "idempotent: nothing re-added");
    }

    // A cache jar whose registry row carries no filename (ingested without a
    // harvest) is named after the requested modid, not an opaque `<sha1>.jar` --
    // the hash-name UX wart depfill used to produce.
    #[tokio::test]
    async fn cache_fallback_without_a_stored_filename_names_by_modid() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", "modb")], NOW)?;
            // None filename: the gap that used to fall back to the sha1
            upsert::upsert_mod_version(
                c,
                id,
                "1.0",
                &["forge"],
                &sha("sha_b"),
                10,
                None,
                None,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                a,
                None,
                "modb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_b"]).await;

        fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert!(
            c.mods.iter().any(|m| m.filename == "modb.jar"),
            "named after the requested modid: {:?}",
            c.mods.iter().map(|m| &m.filename).collect::<Vec<_>>()
        );
        assert!(
            !c.mods.iter().any(|m| m.filename == "sha_b.jar"),
            "the opaque sha1 name is the last resort, not this case"
        );
    }

    // The sticky-dependency contract: a pulled entry survives a save body that
    // lacks it, survives an upstream outage during the fill, and dies exactly
    // when nothing declared reaches it anymore.
    #[tokio::test]
    async fn pulled_dependencies_stick_through_outages_and_prune_as_orphans() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "modb", "1.0", &sha("sha_b"), "modb-1.0.jar");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                a,
                None,
                "modb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();

        // the previously-saved config: curator mod + its pulled dependency
        let mut saved = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        saved.loader.name = "forge".into();
        let mut dep = cache_mod("modb-1.0.jar", &sha("sha_b"));
        dep.source = SourceDecl::SmrtCache { sha1: sha("sha_b") };
        dep.pulled = true;
        saved.mods.push(dep);

        // a stale client body: the curator mod only -- and Modrinth is down
        let mut incoming = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        incoming.loader.name = "forge".into();
        merge_pulled(&saved, &mut incoming);
        assert!(
            incoming.mods.iter().any(|m| m.filename == "modb-1.0.jar"),
            "the pulled dependency is carried over from the saved config"
        );

        let modrinth = Modrinth::with_base("http://127.0.0.1:9").unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_b"]).await;
        fill_dependencies(&mut incoming, &r, &modrinth, &store)
            .await
            .unwrap();
        let kept = incoming
            .mods
            .iter()
            .find(|m| m.filename == "modb-1.0.jar")
            .expect("outage weather must not drop a previously-resolved dependency");
        assert!(kept.pulled, "the sticky marker survives the round trip");

        // a curator-declared mod removed from the body is NOT resurrected
        let mut without_curator = cfg(vec![]);
        without_curator.loader.name = "forge".into();
        merge_pulled(&saved, &mut without_curator);
        assert!(
            !without_curator.mods.iter().any(|m| m.filename == "a.jar"),
            "removing a curator mod is an explicit act"
        );
        // ...and once the dependent is gone, the orphaned pulled dep prunes
        fill_dependencies(&mut without_curator, &r, &modrinth, &store)
            .await
            .unwrap();
        assert!(
            without_curator.mods.is_empty(),
            "a pulled dependency nothing reaches is dropped"
        );
    }

    // merge matches by source identity OR filename, so a re-pin of the same
    // project (new version, new filename) never duplicates the dependency.
    #[test]
    fn merge_does_not_duplicate_a_repinned_project() {
        let mk = |version_id: &str, filename: &str, pulled: bool| DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::Modrinth {
                project_id: "PROJ".into(),
                version_id: version_id.into(),
            },
            display: None,
            slug: None,
            pulled,
        };
        let saved = cfg(vec![mk("v1", "lib-1.0.jar", true)]);
        let mut incoming = cfg(vec![mk("v2", "lib-2.0.jar", false)]);
        merge_pulled(&saved, &mut incoming);
        assert_eq!(
            incoming.mods.len(),
            1,
            "same project under a new pin is the same dependency"
        );
    }

    // One unresolvable target (here: a Modrinth-aliased dep with the API
    // unreachable) must not abort the pass -- the cache-resolvable dependency
    // still fills, and the fill itself reports success.
    #[tokio::test]
    async fn an_unreachable_target_does_not_abort_the_pass() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "modb", "1.0", &sha("sha_b"), "modb-1.0.jar");
        r.with_conn_mut(|c| {
            // netlib resolves through Modrinth (it carries a project alias)
            let netlib = upsert::upsert_mod_by_alias(
                c,
                &[("modid", "netlib"), ("modrinth", "AAAAAAAA")],
                NOW,
            )?;
            let _ = netlib;
            upsert::upsert_relation(
                c,
                a,
                None,
                "netlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                a,
                None,
                "modb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        // nothing listens here: the Modrinth leg fails fast with a connect error
        let modrinth = Modrinth::with_base("http://127.0.0.1:9").unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_b"]).await;

        let added = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .expect("a dead target must not fail the whole fill");
        assert_eq!(added, 1, "the cache-resolvable dependency still fills");
        assert!(
            c.mods.iter().any(|m| m.filename == "modb-1.0.jar"),
            "modb pulled despite the netlib failure"
        );
    }

    // A cached artifact plainly outside the requirer's version window is not
    // pulled; a jar whose bytes are not actually in the cache never is.
    #[tokio::test]
    async fn cache_fallback_respects_window_and_inventory() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "oldlib", "1.0", &sha("sha_old"), "oldlib-1.0.jar");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                a,
                None,
                "oldlib",
                Some("[2.0,)"),
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();

        let (_tmp, store) = cache_holding(&["sha_a", "sha_old"]).await;
        let added = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert_eq!(added, 0, "1.0 is outside [2.0,): not pulled");

        // and without the bytes in the cache inventory, nothing to declare
        let (_tmp, sparse) = cache_holding(&["sha_a"]).await;
        let added = fill_dependencies(&mut c, &r, &modrinth, &sparse)
            .await
            .unwrap();
        assert_eq!(added, 0);
    }

    // The client-mod guard holds on the pull path: an inferred hard edge into a
    // client-side mod pulls nothing, so a client mod can never arrive locked.
    #[tokio::test]
    async fn client_side_dep_is_not_pulled() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "chisel", "1.0", &sha("sha_a"), "chisel.jar");
        add_artifact(&r, "ctm", "1.0", &sha("sha_ctm"), "ctm.jar");
        r.with_conn_mut(|c| {
            upsert::set_jar_class(
                c,
                &sha("sha_ctm"),
                "mod",
                Some("client"),
                Some("tolerant"),
                None,
            )?;
            upsert::upsert_relation(
                c,
                a,
                None,
                "ctm",
                None,
                RelKind::Requires,
                None,
                Source::Inferred,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("chisel.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_ctm"]).await;
        let added = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert_eq!(added, 0, "a client-side mod is never auto-pulled");
        assert_eq!(c.mods.len(), 1);
    }

    // Recommends never lands in the config by itself: it rides the fill plan's
    // suggested list for the panel to offer.
    #[tokio::test]
    async fn recommends_is_suggested_not_added() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let a = add_artifact(&r, "moda", "1.0", &sha("sha_a"), "a.jar");
        add_artifact(&r, "modr", "1.0", &sha("sha_r"), "modr.jar");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                a,
                None,
                "modr",
                None,
                RelKind::Recommends,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        let mut c = cfg(vec![cache_mod("a.jar", &sha("sha_a"))]);
        c.loader.name = "forge".into();
        let modrinth = Modrinth::new().unwrap();
        let (_tmp, store) = cache_holding(&["sha_a", "sha_r"]).await;
        let added = fill_dependencies(&mut c, &r, &modrinth, &store)
            .await
            .unwrap();
        assert_eq!(added, 0, "recommends is never auto-added");
        let plan = r
            .with_conn(|conn| resolve::dependency_fill_plan(conn, &c))
            .unwrap();
        assert_eq!(plan.suggested, vec!["modr".to_string()]);
    }

    /// A canned Modrinth: answers each GET whose path matches a route with that
    /// route's JSON body, 404 otherwise. Enough for the version endpoints the
    /// wire pass and the pinned-dependency leg use, without a mock-server
    /// dependency. Returns the base URL; the task ends with the test.
    async fn stub_modrinth(routes: Vec<(String, String)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    let body = routes
                        .iter()
                        .find(|(p, _)| path.starts_with(p.as_str()))
                        .map(|(_, b)| b.clone());
                    let resp = match body {
                        Some(b) => format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                            b.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string(),
                    };
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        base
    }

    fn version_json(project: &str, id: &str, filename: &str, deps: &str) -> String {
        format!(
            r#"{{"id":"{id}","project_id":"{project}","name":"n","version_number":"1.0",
               "version_type":"release","game_versions":["1.21.1"],"loaders":["neoforge"],
               "files":[{{"hashes":{{"sha1":"{}"}},"url":"http://x/{filename}",
                 "filename":"{filename}","primary":true,"size":10}}],
               "dependencies":[{deps}]}}"#,
            "a".repeat(40)
        )
    }

    // The gap this closes: a mod just picked from Modrinth is unknown to the
    // registry (the harvest reads the cache and published manifests, neither of
    // which has seen it), so its libraries used to go unpulled until the pack had
    // been built once. Modrinth states the dependency on the version itself, and
    // the fill now reads it there.
    #[tokio::test]
    async fn an_unharvested_pin_pulls_its_dependency_from_the_wire() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let pin = version_json(
            "PROJ_A",
            "VER_A",
            "a.jar",
            r#"{"project_id":"PROJ_LIB","version_id":"VER_LIB","dependency_type":"required"},
               {"project_id":"PROJ_OPT","dependency_type":"optional"}"#,
        );
        let lib = version_json("PROJ_LIB", "VER_LIB", "lib-1.0.jar", "");
        let base = stub_modrinth(vec![
            // the batched pin lookup the wire pass makes
            ("/v2/versions".to_string(), format!("[{pin},{lib}]")),
            // the pinned dependency, resolved at the exact version it names
            (
                "/v2/project/PROJ_LIB/version/VER_LIB".to_string(),
                lib.clone(),
            ),
        ])
        .await;
        let modrinth = Modrinth::with_base(&base).unwrap();

        let mut c = cfg(vec![DeclaredMod {
            filename: "a.jar".into(),
            default_enabled: true,
            source: SourceDecl::Modrinth {
                project_id: "PROJ_A".into(),
                version_id: "VER_A".into(),
            },
            display: None,
            slug: None,
            pulled: false,
        }]);

        let added = fill_dependencies(&mut c, &r, &modrinth, &empty_cache().1)
            .await
            .unwrap();
        assert_eq!(
            added, 1,
            "the required dependency is pulled; the optional is not"
        );
        let lib = c
            .mods
            .iter()
            .find(|m| m.filename == "lib-1.0.jar")
            .expect("dependency added under its real filename");
        assert!(lib.pulled);
        assert!(
            matches!(&lib.source, SourceDecl::Modrinth { project_id, version_id }
                if project_id == "PROJ_LIB" && version_id == "VER_LIB"),
            "the exact version the requirer pinned is honoured"
        );
        // the requires edge is recorded, so the build locks the pulled library
        let reqs = &c.mods[0].display.as_ref().unwrap().requires;
        assert_eq!(reqs[0].filename, "lib-1.0.jar");

        let again = fill_dependencies(&mut c, &r, &modrinth, &empty_cache().1)
            .await
            .unwrap();
        assert_eq!(again, 0, "idempotent");
    }

    // Upstream sometimes publishes a version whose jar never landed (metadata
    // listed, `files` empty). Pinning one only fails at build time, so it is not
    // a candidate however new it is.
    #[test]
    fn a_version_without_a_file_is_never_usable() {
        let with_file: MrVersion =
            serde_json::from_str(&version_json("P", "V", "m.jar", "")).unwrap();
        assert!(usable(&with_file, "neoforge"));
        assert!(!usable(&with_file, "fabric"), "wrong loader");

        let mut fileless = with_file.clone();
        fileless.files.clear();
        assert!(
            !usable(&fileless, "neoforge"),
            "a version upstream published without a jar is not a candidate"
        );
    }

    #[test]
    fn apply_requires_records_edges_and_leaves_leaves_bare() {
        let mut c = cfg(vec![one("a.jar"), one("lib.jar")]);
        apply_requires(&mut c, &[("a.jar".into(), "lib.jar".into())]);
        let a = c.mods.iter().find(|m| m.filename == "a.jar").unwrap();
        let reqs = &a.display.as_ref().unwrap().requires;
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].filename, "lib.jar");
        assert!(!reqs[0].optional);
        // a mod that depends on nothing does not get a display materialized just to
        // hold an empty requires list
        let lib = c.mods.iter().find(|m| m.filename == "lib.jar").unwrap();
        assert!(lib.display.is_none());
    }
}
