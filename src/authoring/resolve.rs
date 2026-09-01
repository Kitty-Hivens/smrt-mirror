//! The resolve pass: read the registry dependency graph for a pack's declared
//! mods and report the problems a build would otherwise only surface at crash
//! time -- an unmet hard dependency, an active conflict, a same-capability
//! overlap, or a present dependency whose version falls outside the window a
//! requirer declared.
//!
//! Pure over a `&Connection` (the handler runs it inside `spawn_blocking` via
//! `Registry::with_conn`). It never mutates the config; a mod's required-ness is
//! derived from the dependency graph at build time, not set here. When it cannot
//! decide something confidently -- a mod with no
//! registry identity, a version string it cannot compare against a window -- it
//! reports that as unresolved/unchecked rather than guess, so a flagged problem
//! is a real one.
//!
//! Edges are read at artifact granularity (#48): a pack declares a file, so the
//! facts that apply are the ones that file declares, plus whatever is asserted
//! about its mod as a whole. A jar the registry has never read gets the mod-level
//! facts only -- it does not borrow a sibling version's dependencies.

use crate::domain::{PackConfig, SideClass, SourceDecl};
use crate::registry::classify::{Classification, classify_artifact};
use crate::registry::model::{GraphData, GraphEdge, RelKind, Severity, Source};
use crate::registry::{queries, semver};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use ts_rs::TS;

/// The outcome of resolving a pack against the registry graph. Arrays are empty
/// when clean; `missing` and `conflicts` are the blocking problems, the rest are
/// advisory. All lists are sorted for a stable render.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ResolveReport {
    #[ts(type = "number")]
    pub declared_mods: usize,
    /// How many declared jar mods are identified: a registry identity, or a valid
    /// Modrinth pin the mirror has not harvested yet (present at build). The rest
    /// are in `unresolved`.
    #[ts(type = "number")]
    pub resolved_mods: usize,
    /// A hard dependency no present mod satisfies -- the pack would crash.
    pub missing: Vec<MissingDep>,
    /// Two present mods the graph says cannot run together, both in the default
    /// install -- a live conflict the pack ships with.
    pub conflicts: Vec<ActiveConflict>,
    /// The same incompatibility, but with at least one side an opted-out optional
    /// -- it only bites if the user turns that mod on, so it is advisory, not a
    /// blocking problem (#9).
    pub optional_conflicts: Vec<ActiveConflict>,
    /// A capability more than one present mod provides -- usually redundant, and
    /// the two may fight over the same hook.
    pub overlaps: Vec<CapabilityOverlap>,
    /// A present dependency whose shipped version sits outside a requirer's
    /// declared window.
    pub version_issues: Vec<VersionIssue>,
    /// The same finding against the loader itself: a jar declaring a build of
    /// the pack's own loader that the pinned one does not satisfy (#164).
    ///
    /// Filled by [`super::loaderreq::loader_windows`], not by [`resolve_pack`]:
    /// the window is inside the jar, and a Modrinth pin's jar is not on this
    /// disk, so answering needs I/O this pure pass has not got. A caller that
    /// does not run that pass leaves it empty, which reads as "not checked" --
    /// the same as every other fact the mirror has not looked up.
    #[serde(default)]
    pub loader_version_issues: Vec<LoaderVersionIssue>,
    /// Declared artifacts this pack's loader cannot run, with nothing present to
    /// bridge them -- they will not load at all (#50).
    pub loader_mismatch: Vec<LoaderMismatch>,
    /// A required mixin whose target the pack's copy of the host no longer
    /// carries (#145). The pack starts loading and dies during init.
    pub mixin_gaps: Vec<MixinGap>,
    /// Foreign-loader artifacts a present connector carries. Not a problem: they
    /// load. Listed because it is worth knowing which mods in a forge pack are
    /// actually fabric mods riding the bridge -- pull the connector and they all
    /// go at once.
    pub loader_bridged: Vec<LoaderMismatch>,
    /// Declared jar mods with no identity and no valid pin: an un-harvested
    /// `smrt_cache` upload the mirror has not read. Left unjudged, listed so the
    /// operator knows coverage was partial. A Modrinth pin is not here -- it is a
    /// valid declaration, counted in `resolved_mods`.
    pub unresolved: Vec<String>,
    /// How many version windows could not be checked because a version string was
    /// not plainly comparable (a classifier suffix, an embedded MC prefix).
    #[ts(type = "number")]
    pub version_windows_unchecked: usize,
    /// Declared jars that are not mods at all (bare coremods / ASM libraries):
    /// always toggleable, never lockable -- listed so the curator knows why.
    pub coremods: Vec<String>,
    /// Mods the classifier could not decide a match policy for: shipped
    /// toggleable rather than guessed. Curator material, not an error.
    pub unclassified: Vec<String>,
    /// Modrinth environment flags and the bytecode derivation disagree on the
    /// side. The flags win by priority, but they are authored upstream and
    /// sometimes wrong -- the curator should see the conflict.
    pub side_disagreements: Vec<SideDisagreement>,
    /// A declared (non-inferred) hard dependency targets a client-side mod --
    /// a data error the build will refuse: a client mod is never
    /// force-installed. Inferred edges never get here (the guard downgrades
    /// them to soft before they can lock anything).
    pub forced_client_attempts: Vec<ForcedClientEdge>,
    /// Server-side mods in the pack: legitimate, but the client manifest ships
    /// them opted out (never required, default-disabled).
    pub server_side: Vec<String>,
    /// `Recommends` targets absent from the pack -- curator suggestions with a
    /// manual add action, never auto-added.
    pub suggestions: Vec<String>,
}

/// One Modrinth-vs-bytecode side conflict on a declared mod.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SideDisagreement {
    pub filename: String,
    /// The winning side per the Modrinth environment flags.
    pub modrinth_side: String,
    /// What the bytecode derivation concluded instead.
    pub bytecode_side: String,
}

/// A declared hard edge that would force-install a client-side mod.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ForcedClientEdge {
    /// The client-side mod being forced.
    pub filename: String,
    /// The declaring mods.
    pub needed_by: Vec<String>,
    /// Provenance of the offending edge (jar-meta / modrinth / authored / curator).
    pub source: String,
}

/// A required target no present mod satisfies. `target` is the graph selector
/// (a modid, or `modrinth:<project_id>`); `needed_by` are the filenames that
/// require it; `source` is the provenance of the authoritative edge.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MissingDep {
    pub target: String,
    pub needed_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version_range: Option<String>,
    pub source: String,
    /// Why the dependency cannot be satisfied automatically, when known:
    /// `external` -- a Modrinth dependency naming only a file, living outside
    /// both Modrinth and the mirror. Not a resolver bug; curator material.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

/// Two present mods the graph marks incompatible.
///
/// `hard` is the declaration's own intensity (#129): the loader refuses to run
/// the two together, as against an author advising against it. An edge that
/// somehow carries no severity reads as hard -- an incompatibility of unknown
/// intensity is the one to show, not the one to quietly play down.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ActiveConflict {
    pub a: String,
    pub b: String,
    pub hard: bool,
    pub source: String,
}

/// A jar's `required` mixin config names a class that the pack's own copy of the
/// mod owning it does not have.
///
/// Fatal, and invisible in every declaration: both mods that hit it declared an
/// open lower bound on the host, which every version satisfies. The loader
/// throws during init and blames whichever mod first reached the missing class,
/// so the crash report names a bystander.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MixinGap {
    /// The jar whose mixin config asks for it.
    pub filename: String,
    /// The config that declares it (`sable.mixins.json`).
    pub config: String,
    /// The missing class, as a binary name.
    pub needed: String,
    /// The pack's jar that owns the class's package and does not carry it.
    pub owner: String,
}

/// A capability more than one present mod provides.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CapabilityOverlap {
    pub capability: String,
    pub mods: Vec<String>,
}

/// A present dependency shipping a version outside a declared window.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VersionIssue {
    pub target: String,
    pub filename: String,
    pub present_version: String,
    pub required_range: String,
    pub needed_by: Vec<String>,
}

/// A jar whose declared loader window the pack's pinned loader build falls
/// outside of -- JEI's `neoforge [21.1.238,)` under a pack pinned to
/// `21.1.234`. The loader reads that window out of the jar and refuses to
/// start, naming the jar; nothing says which build it wanted, or that the pin
/// is a line in this pack's own config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LoaderVersionIssue {
    pub filename: String,
    /// The loader both sides name, as the mirror spells it.
    pub loader: String,
    /// The build this pack pins.
    pub pack_version: String,
    /// The window the jar declares, verbatim.
    pub required_range: String,
}

/// A declared artifact built for loaders this pack does not run.
///
/// A pack natively runs its own loader and everything that loader inherits from
/// (cleanroom runs forge's artifacts, quilt runs fabric's). Anything else needs a
/// bridge -- a connector mod that carries another loader's mods at runtime, which
/// the registry records as a `provides` of the `loader:<name>` capability.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LoaderMismatch {
    pub filename: String,
    /// the loaders the artifact was actually published for
    pub artifact_loaders: Vec<String>,
    pub pack_loader: String,
    /// The present mod bridging one of those loaders, when there is one. A bridge
    /// carries the mod -- what it does not promise is how stable the result is,
    /// and that rides on the connector rather than on any one mod, so it is not
    /// something this pass can judge per mod (and does not try to).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bridged_by: Option<String>,
}

/// A dependency on the loader itself (Forge/FML, NeoForge, Fabric, ...), which is
/// always present, so it is not a missing mod however the jar spells it -- Forge
/// mods variously require `forge`, `MinecraftForge`, `mod_MinecraftForge` or
/// `FML`. Any version window rides after `@` and is dropped first. A jar's loader
/// compatibility is judged separately (#50); this only stops the scanner
/// reporting the loader as a missing dependency (#10).
fn is_loader_dep(target: &str) -> bool {
    let id = target
        .split('@')
        .next()
        .unwrap_or(target)
        .to_ascii_lowercase();
    matches!(
        id.as_str(),
        "forge"
            | "minecraftforge"
            | "mod_minecraftforge"
            | "fml"
            | "forgemodloader"
            | "neoforge"
            | "fabric"
            | "fabricloader"
            | "cleanroom"
            | "quilt"
    )
}

/// Dependency selectors naming Fabric API, in the spellings one can arrive in:
/// the Modrinth project a Modrinth-declared edge names, and the id a
/// `fabric.mod.json` declares. `fabric` on its own is the loader, which
/// `is_loader_dep` already answers.
const FABRIC_API: [&str; 2] = ["modrinth:P7dR8mSH", "fabric-api"];

/// Selectors naming Forgified Fabric API: its Modrinth project, and the modid
/// its jar declares.
const FORGIFIED_FABRIC_API: [&str; 2] = ["modrinth:Aqlf1Shp", "fabric_api"];

/// Dependency selectors a present mod answers with no edge in the graph saying
/// so.
///
/// A fabric mod carried onto a Forge-family loader by a connector still declares
/// `fabric-api`, and what answers it on that side is Forgified Fabric API -- a
/// separate project with its own modid, so nothing joins the two and the
/// dependency reads as unmet on a pack that runs it perfectly well. The
/// equivalence belongs to the bridge rather than to any one pack, so it lives
/// here beside the loader spellings instead of as a row every mirror has to
/// author for itself.
///
/// Deliberately not conditioned on a connector being present: without one the
/// fabric mod does not load at all, which `loader_mismatch` reports on its own,
/// and calling its dependency missing on top of that names two problems where
/// there is one.
fn bridged_api_provided(
    conn: &Connection,
    by_mod_id: &HashMap<i64, usize>,
) -> Result<HashSet<String>> {
    let mut provided = HashSet::new();
    for selector in FORGIFIED_FABRIC_API {
        if let Some(id) = queries::mod_id_for_selector(conn, selector)?
            && by_mod_id.contains_key(&id)
        {
            provided.extend(FABRIC_API.iter().map(|s| s.to_string()));
            break;
        }
    }
    Ok(provided)
}

/// A declared jar mod placed on the graph.
struct Present {
    filename: String,
    /// Ships enabled unless the operator opted it out. Every mod is toggleable
    /// (there is no hand-set required flag), so this alone decides the default
    /// install -- a conflict only bites when both sides actually run (#9).
    default_enabled: bool,
    mod_id: i64,
    version: Option<String>,
    /// The exact artifact the pack ships, when the registry has read it. A pack
    /// declares a file (by sha1, or by Modrinth version id), so its dependencies
    /// are that file's -- not the union of every version of its mod (#48). `None`
    /// when the artifact was never harvested: then only mod-level facts apply,
    /// since we have never actually looked inside this jar.
    mod_version_id: Option<i64>,
    /// The declared content hash (a `smrt_cache` declaration, or the harvested
    /// artifact behind a Modrinth pin) -- the classification key.
    sha1: Option<String>,
}

/// Place each declared jar mod on the registry graph, returning the ones that
/// landed plus the filenames that could not be identified. A `SmrtStatic` source
/// is not a mod (a config/asset file) and is skipped; a jar with no registry
/// identity cannot be reasoned about and is reported unresolved.
fn place_mods(conn: &Connection, cfg: &PackConfig) -> Result<PlacedMods> {
    let mut present: Vec<Present> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut non_mods: Vec<String> = Vec::new();
    let mut pinned_projects: Vec<String> = Vec::new();
    for m in &cfg.mods {
        let (mod_id, version, mod_version_id, sha1) = match &m.source {
            SourceDecl::SmrtCache { sha1 } => match queries::artifact_by_sha1(conn, sha1)? {
                Some((mv_id, id, ver)) => (id, Some(ver), Some(mv_id), Some(sha1.clone())),
                None => {
                    // No identity, but the harvest may still have read the jar:
                    // a bare coremod/ASM library (ChickenASM-class) is exactly
                    // that, and it is classified, not "unresolved".
                    match queries::jar_class_for_sha1(conn, sha1)? {
                        Some(jc) if jc.kind != "mod" => non_mods.push(m.filename.clone()),
                        _ => unresolved.push(m.filename.clone()),
                    }
                    continue;
                }
            },
            SourceDecl::Modrinth {
                project_id,
                version_id,
            } => match queries::mod_id_for_alias(conn, "modrinth", project_id)? {
                Some(id) => {
                    let mv_id = queries::mod_version_id_for_modrinth_version_id(conn, version_id)?;
                    let sha = match mv_id {
                        Some(mv) => queries::sha1_for_mod_version(conn, mv)?,
                        None => None,
                    };
                    (
                        id,
                        queries::version_by_modrinth_version_id(conn, version_id)?,
                        mv_id,
                        sha,
                    )
                }
                // A Modrinth pin the mirror has not harvested yet is still valid: a
                // build fetches it straight from Modrinth, so it will be present.
                // This is a pre-build check, so it must not be reported as an
                // unidentified mod. Record the project instead -- a
                // `modrinth:<project>` dependency resolves against it -- and leave
                // its own dependencies unchecked (nothing is harvested to read).
                None => {
                    pinned_projects.push(project_id.clone());
                    continue;
                }
            },
            SourceDecl::SmrtStatic { .. } => continue,
        };
        present.push(Present {
            filename: m.filename.clone(),
            default_enabled: m.default_enabled,
            mod_id,
            version,
            mod_version_id,
            sha1,
        });
    }
    Ok(PlacedMods {
        present,
        unresolved,
        non_mods,
        pinned_projects,
    })
}

/// The outcome of placing a pack's declared mods on the registry graph.
struct PlacedMods {
    /// Mods the registry has an identity for -- reasoned about fully.
    present: Vec<Present>,
    /// Declared mods with no identity and no valid pin: an un-harvested `smrt_cache`
    /// upload. Listed, not judged.
    unresolved: Vec<String>,
    /// Identity-less jars the harvest read and classified as not-a-mod (bare
    /// coremods / ASM libraries) -- the coremod advisory, not "unresolved".
    non_mods: Vec<String>,
    /// Modrinth project ids of declared Modrinth mods the mirror has not harvested.
    /// Valid pins (present at build); a `modrinth:<project>` dependency they cover
    /// is satisfied even though their own edges cannot be walked.
    pinned_projects: Vec<String>,
}

/// What the dependency-fill pass on config save needs from the registry: the hard
/// dependencies a pack's mods declare, split into the ones already satisfied by a
/// present mod (an edge, for `display.requires`) and the ones nothing satisfies
/// (candidates to auto-add).
pub struct DepFillPlan {
    /// Hard dependencies no present mod covers -- the auto-pull candidates.
    pub missing: Vec<MissingTarget>,
    /// `(mod filename, its hard-dep filename)` among present mods -- the graph
    /// `display.requires` records so the build can derive required-ness.
    pub requires: Vec<(String, String)>,
    /// `Recommends` targets absent from the pack: shown to the curator as
    /// suggestions with a manual add action, never auto-added.
    pub suggested: Vec<String>,
}

/// One unsatisfied hard dependency: the graph selector plus the requirer's
/// version window (checked against a cache candidate before auto-adding it).
#[derive(Debug, Clone)]
pub struct MissingTarget {
    pub selector: String,
    pub version_range: Option<String>,
    /// The exact Modrinth version the requirer names, when it names one. A
    /// pinned dependency is honoured as pinned rather than resolved to the
    /// newest compatible build. Registry edges carry a range, not a pin, so this
    /// is set only by the wire-dependency pass.
    pub pinned_version: Option<String>,
}

/// Classify each declared jar mod of the pack through the decision layer,
/// keyed by filename -- the map the build consumes to seed required-ness and
/// emit presence. An identity-less jar the harvest still read (a bare
/// coremod/library) classifies by its jar_class row alone, so its coremod
/// presence reaches the manifest; a jar the registry knows nothing about is
/// absent (unclassified).
pub fn classify_pack(
    conn: &Connection,
    cfg: &PackConfig,
) -> Result<HashMap<String, Classification>> {
    let placed = place_mods(conn, cfg)?;
    let mut out = HashMap::new();
    for p in &placed.present {
        let c = classify_artifact(conn, Some(p.mod_id), p.sha1.as_deref())?;
        out.insert(p.filename.clone(), c);
    }
    for m in &cfg.mods {
        if out.contains_key(&m.filename) {
            continue;
        }
        if let SourceDecl::SmrtCache { sha1 } = &m.source {
            let c = classify_artifact(conn, None, Some(sha1))?;
            if c.kind.is_some() {
                out.insert(m.filename.clone(), c);
            }
        }
    }
    Ok(out)
}

/// The side-based downgrade of an inferred hard edge. A bytecode-inferred
/// "requires" is class-granularity evidence: it cannot tell a hard dependency
/// from an `@Optional.Interface` integration that references a foreign mod's API
/// type in ordinary tile/item code. So an inferred edge to a *classified* target
/// is treated as soft before it can lock, pull, or report the target missing --
/// a real hard dependency arrives on a declared tier (mcmod.info `requiredMods`,
/// a loader manifest, or Modrinth), which is never downgraded.
///
/// Client and server targets were the original guard (a real cross-side hard dep
/// would crash a dedicated server / never belongs in a client pack). `Both`
/// joins them: an inferred edge onto a both-side content mod -- Mekanism,
/// Draconic Evolution -- is the same false positive (a mod implementing their
/// energy API), and force-pulling a large content mod off a bytecode reference
/// dirties every self-hosted pack. Only inferred edges downgrade; a declared
/// edge to a client mod stays a data error the resolve report surfaces instead.
fn inferred_edge_downgraded(edge_source: Source, target_class: Option<&Classification>) -> bool {
    edge_source == Source::Inferred
        && matches!(
            target_class.and_then(|c| c.side),
            Some(SideClass::Client) | Some(SideClass::Server) | Some(SideClass::Both)
        )
}

/// Mod-level classification of an edge target that may not be in the pack:
/// the mod's Modrinth flags plus its newest scanned artifact's verdict.
fn classify_target_mod(conn: &Connection, mod_id: i64) -> Result<Classification> {
    let sha = queries::newest_sha1_for_mod(conn, mod_id)?;
    classify_artifact(conn, Some(mod_id), sha.as_deref())
}

/// Walk each present mod's authoritative hard-dependency edges and classify each:
/// satisfied by a present mod (an edge) or unsatisfied (missing). A loader dep is
/// never missing, and a `modrinth:<project>` a declared Modrinth pin covers counts
/// as satisfied even before the pin is harvested (it is present at build).
pub fn dependency_fill_plan(conn: &Connection, cfg: &PackConfig) -> Result<DepFillPlan> {
    let placed = place_mods(conn, cfg)?;
    let pinned: HashSet<&str> = placed.pinned_projects.iter().map(String::as_str).collect();
    let mut by_mod_id: HashMap<i64, usize> = HashMap::new();
    for (i, p) in placed.present.iter().enumerate() {
        by_mod_id.entry(p.mod_id).or_insert(i);
    }
    let loader_provided = queries::loader_provided_capabilities(conn, &cfg.loader.name)?;
    let bridged_provided = bridged_api_provided(conn, &by_mod_id)?;
    // target-mod classifications, resolved lazily once per mod id
    let mut target_class: HashMap<i64, Classification> = HashMap::new();
    let mut missing: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut requires: Vec<(String, String)> = Vec::new();
    let mut suggested: BTreeSet<String> = BTreeSet::new();
    for a in &placed.present {
        let mut seen: HashSet<String> = HashSet::new();
        for e in queries::relations_for_artifact(conn, a.mod_version_id.unwrap_or(-1), a.mod_id)? {
            if !seen.insert(format!("{}\x1f{}", e.kind.as_str(), e.target)) {
                continue;
            }
            // a Recommends target the pack lacks is a curator suggestion,
            // never an auto-add
            if e.kind == RelKind::Recommends {
                let absent = queries::mod_id_for_selector(conn, &e.target)?
                    .and_then(|id| by_mod_id.get(&id))
                    .is_none();
                if absent && !is_loader_dep(&e.target) {
                    suggested.insert(e.target.clone());
                }
                continue;
            }
            if e.kind != RelKind::Requires {
                continue;
            }
            if is_loader_dep(&e.target) {
                continue;
            }
            // a capability the loader ships natively is answered by the loader:
            // do not record it as an auto-pull candidate (it would pull back a mod
            // the fork makes redundant)
            if loader_provided.contains(e.target.split('@').next().unwrap_or(&e.target)) {
                continue;
            }
            // likewise a fabric-side API the pack answers with its forge-family
            // port: satisfied, so there is nothing to pull
            if bridged_provided.contains(e.target.split('@').next().unwrap_or(&e.target)) {
                continue;
            }
            let target_mod = queries::mod_id_for_selector(conn, &e.target)?;
            if let Some(tid) = target_mod {
                if let std::collections::hash_map::Entry::Vacant(v) = target_class.entry(tid) {
                    v.insert(classify_target_mod(conn, tid)?);
                }
                // the client/server guard: an inferred hard edge into a sided
                // mod neither records a requires edge nor pulls the target
                if inferred_edge_downgraded(e.source, target_class.get(&tid)) {
                    continue;
                }
            }
            match target_mod.and_then(|id| by_mod_id.get(&id)) {
                Some(&bi) => {
                    requires.push((a.filename.clone(), placed.present[bi].filename.clone()))
                }
                None => {
                    if let Some(pid) = e.target.strip_prefix("modrinth:")
                        && pinned.contains(pid.split('@').next().unwrap_or(pid))
                    {
                        continue;
                    }
                    // first requirer's window wins (highest-confidence edge)
                    missing
                        .entry(e.target.clone())
                        .or_insert(e.version_range.clone());
                }
            }
        }
    }
    Ok(DepFillPlan {
        missing: missing
            .into_iter()
            .map(|(selector, version_range)| MissingTarget {
                selector,
                version_range,
                pinned_version: None,
            })
            .collect(),
        requires,
        suggested: suggested.into_iter().collect(),
    })
}

/// The relation graph of one pack: its own mods, wired by the relations the exact
/// artifacts it ships declare.
///
/// The same edges mean something different here than in the registry-wide graph.
/// Globally "X requires forge" is noise; inside a pack the question is whether the
/// thing required is actually here. So a target is resolved only when the pack
/// ships that mod -- anything else stays dangling, and the panel can read a
/// dangling `requires` as a missing dependency and a `conflicts` between two
/// present mods as a live one, off the same shape the registry graph already uses.
///
/// Unlike the registry graph this keeps every declared mod as a node, isolated or
/// not: a mod with no relations is still part of the pack, and this is a picture of
/// the pack's composition rather than of the relation web.
pub fn pack_graph(conn: &Connection, cfg: &PackConfig) -> Result<GraphData> {
    let present = place_mods(conn, cfg)?.present;
    let in_pack: HashSet<i64> = present.iter().map(|p| p.mod_id).collect();

    let mut nodes = Vec::with_capacity(present.len());
    let mut seen: HashSet<i64> = HashSet::new();
    for p in &present {
        if seen.insert(p.mod_id) {
            nodes.push(queries::graph_node_for(conn, p.mod_id)?);
        }
    }

    let mut edges = Vec::new();
    for p in &present {
        for e in queries::relations_for_artifact(conn, p.mod_version_id.unwrap_or(-1), p.mod_id)? {
            let to = queries::mod_id_for_selector(conn, &e.target)?.filter(|t| in_pack.contains(t));
            edges.push(GraphEdge {
                from_mod_id: p.mod_id,
                to_mod_id: to,
                target: e.target,
                kind: e.kind.as_str().to_string(),
                severity: e.severity.map(|s| s.as_str().to_string()),
                source: e.source.as_str().to_string(),
            });
        }
    }
    Ok(GraphData { nodes, edges })
}

/// Resolve `cfg` against the registry graph reachable through `conn`.
pub fn resolve_pack(conn: &Connection, cfg: &PackConfig) -> Result<ResolveReport> {
    // 1. Place each declared jar mod on the graph.
    let PlacedMods {
        present,
        mut unresolved,
        non_mods,
        pinned_projects,
    } = place_mods(conn, cfg)?;
    // Modrinth projects the pack pins but the mirror has not harvested: a
    // `modrinth:<project>` dependency they cover is satisfied (present at build).
    let pinned: HashSet<&str> = pinned_projects.iter().map(String::as_str).collect();

    // first declaration of a mod_id wins the index (a pack rarely ships one mod
    // twice; if it does, the earlier row is the one findings point at)
    let mut by_mod_id: HashMap<i64, usize> = HashMap::new();
    for (i, p) in present.iter().enumerate() {
        by_mod_id.entry(p.mod_id).or_insert(i);
    }

    // Classification per mod id: the present mods' artifacts up front, absent
    // edge targets lazily as the walk reaches them.
    let mut class_of: HashMap<i64, Classification> = HashMap::new();
    for p in &present {
        if let std::collections::hash_map::Entry::Vacant(v) = class_of.entry(p.mod_id) {
            v.insert(classify_artifact(conn, Some(p.mod_id), p.sha1.as_deref())?);
        }
    }

    // 2. Walk each present mod's authoritative edges.
    let mut missing: BTreeMap<String, MissingDep> = BTreeMap::new();
    let mut conflicts: Vec<ActiveConflict> = Vec::new();
    let mut optional_conflicts: Vec<ActiveConflict> = Vec::new();
    let mut conflict_seen: HashSet<(usize, usize)> = HashSet::new();
    let mut provides: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut version_issues: Vec<VersionIssue> = Vec::new();
    let mut forced_client: BTreeMap<String, ForcedClientEdge> = BTreeMap::new();
    let mut suggestions: BTreeSet<String> = BTreeSet::new();
    let mut unchecked = 0usize;

    for (ai, a) in present.iter().enumerate() {
        // Scoped to the artifact the pack actually ships, plus the mod-level facts
        // (#48): a sibling version's dependencies are not this file's. `-1` matches
        // no artifact, which is how an unharvested jar falls back to mod-level
        // facts alone rather than borrowing another build's.
        //
        // Ordered by confidence, so the first edge per target is the authoritative
        // one -- an authored optional_dep suppresses an inferred requires for the
        // same target, and so on.
        let mut seen_target: HashSet<String> = HashSet::new();
        for e in queries::relations_for_artifact(conn, a.mod_version_id.unwrap_or(-1), a.mod_id)? {
            if !seen_target.insert(e.target.clone()) {
                continue;
            }
            match e.kind {
                RelKind::Requires => {
                    let target_mod = queries::mod_id_for_selector(conn, &e.target)?;
                    if let Some(tid) = target_mod {
                        if let std::collections::hash_map::Entry::Vacant(v) = class_of.entry(tid) {
                            v.insert(classify_target_mod(conn, tid)?);
                        }
                        // the client/server guard: an inferred hard edge into a
                        // sided mod is soft -- it neither reports the target
                        // missing nor (downstream) locks it required
                        if inferred_edge_downgraded(e.source, class_of.get(&tid)) {
                            continue;
                        }
                    }
                    let tgt_present = target_mod.and_then(|id| by_mod_id.get(&id).copied());
                    match tgt_present {
                        Some(bi) => {
                            let b = &present[bi];
                            // A DECLARED hard edge from a non-client mod into
                            // a client-side one: the build will not lock the
                            // target (client mods are never force-installed),
                            // so the dependency cannot be enforced -- surface
                            // the pair. A client mod requiring another client
                            // mod is a normal launcher-co-toggled chain and is
                            // not reported.
                            if e.source != crate::registry::model::Source::Inferred
                                && class_of.get(&b.mod_id).and_then(|c| c.side)
                                    == Some(SideClass::Client)
                                && class_of.get(&a.mod_id).and_then(|c| c.side)
                                    != Some(SideClass::Client)
                            {
                                let entry =
                                    forced_client.entry(b.filename.clone()).or_insert_with(|| {
                                        ForcedClientEdge {
                                            filename: b.filename.clone(),
                                            needed_by: Vec::new(),
                                            source: e.source.as_str().to_string(),
                                        }
                                    });
                                entry.needed_by.push(a.filename.clone());
                            }
                            if let Some(range) = e.version_range.as_deref() {
                                match b
                                    .version
                                    .as_deref()
                                    .and_then(|v| semver::in_range(v, range))
                                {
                                    Some(true) => {}
                                    Some(false) => version_issues.push(VersionIssue {
                                        target: e.target.clone(),
                                        filename: b.filename.clone(),
                                        present_version: b.version.clone().unwrap_or_default(),
                                        required_range: range.to_string(),
                                        needed_by: vec![a.filename.clone()],
                                    }),
                                    None => unchecked += 1,
                                }
                            }
                        }
                        None => {
                            // A dependency on the loader itself is never a missing
                            // mod -- the loader is always present (#10).
                            if is_loader_dep(&e.target) {
                                continue;
                            }
                            // A `modrinth:<project>` the pack pins but the mirror has
                            // not harvested is satisfied by that pin -- the build
                            // fetches it, so it is present, not missing.
                            if let Some(pid) = e.target.strip_prefix("modrinth:") {
                                let pid = pid.split('@').next().unwrap_or(pid);
                                if pinned.contains(pid) {
                                    continue;
                                }
                            }
                            let entry =
                                missing
                                    .entry(e.target.clone())
                                    .or_insert_with(|| MissingDep {
                                        reason: e
                                            .target
                                            .starts_with("external:")
                                            .then(|| "external".to_string()),
                                        target: e.target.clone(),
                                        needed_by: Vec::new(),
                                        version_range: e.version_range.clone(),
                                        source: e.source.as_str().to_string(),
                                    });
                            entry.needed_by.push(a.filename.clone());
                        }
                    }
                }
                RelKind::Conflicts => {
                    if let Some(bi) = queries::mod_id_for_selector(conn, &e.target)?
                        .and_then(|id| by_mod_id.get(&id).copied())
                    {
                        let pair = if ai < bi { (ai, bi) } else { (bi, ai) };
                        if conflict_seen.insert(pair) {
                            let c = ActiveConflict {
                                a: a.filename.clone(),
                                b: present[bi].filename.clone(),
                                hard: e.severity != Some(Severity::Soft),
                                source: e.source.as_str().to_string(),
                            };
                            // Both in the default install -> a live conflict; an
                            // opted-out mod on either side makes it advisory, firing
                            // only if the user enables that mod (#9).
                            let b = &present[bi];
                            let both_on = a.default_enabled && b.default_enabled;
                            if both_on {
                                conflicts.push(c);
                            } else {
                                optional_conflicts.push(c);
                            }
                        }
                    }
                }
                RelKind::Provides => {
                    provides
                        .entry(e.target.clone())
                        .or_default()
                        .insert(a.filename.clone());
                }
                // a Recommends target the pack lacks is a curator suggestion
                RelKind::Recommends => {
                    let absent = queries::mod_id_for_selector(conn, &e.target)?
                        .and_then(|id| by_mod_id.get(&id))
                        .is_none();
                    if absent && !is_loader_dep(&e.target) {
                        suggestions.insert(e.target.clone());
                    }
                }
                // a soft dependency absent from the pack is the normal case, not a
                // problem to report
                RelKind::OptionalDep => {}
            }
        }
    }

    // A required target a present mod `provides` as a capability is satisfied.
    // So is one the loader ships natively -- a modern fork bundling what is a
    // separate mod on Forge (Cleanroom loads mixins, so a MixinBooter dependency
    // is met with no MixinBooter jar in the pack). So, finally, is a bridged
    // loader's API the pack answers with its own side's port. The `@version`
    // suffix a selector may carry is not part of the capability key.
    let loader_provided = queries::loader_provided_capabilities(conn, &cfg.loader.name)?;
    let bridged_provided = bridged_api_provided(conn, &by_mod_id)?;
    let bare = |t: &str| t.split('@').next().unwrap_or(t).to_string();
    missing.retain(|target, _| {
        !provides.contains_key(target)
            && !loader_provided.contains(&bare(target))
            && !bridged_provided.contains(&bare(target))
    });

    // Loader eligibility (#50). A pack natively runs its own loader and whatever
    // that loader inherits from; anything else needs a bridge. A bridge is a
    // present mod that `provides` the `loader:<name>` capability -- a connector.
    //
    // A connector carries the mod: bridged is not a warning, and does not spoil a
    // clean report. What a bridge does not promise is stability, and that depends
    // on the connector rather than on any one mod -- there is nothing to derive per
    // mod, so nothing is claimed. Listing them is still worth it: these are the
    // mods that all go at once if the connector leaves the pack.
    let chain = queries::loader_chain(conn, &cfg.loader.name)?;
    let mut loader_mismatch: Vec<LoaderMismatch> = Vec::new();
    let mut loader_bridged: Vec<LoaderMismatch> = Vec::new();
    for a in &present {
        // never read the jar -> its loaders are unknown, so there is nothing to judge
        let Some(mv) = a.mod_version_id else { continue };
        let targets = queries::targets_for_artifact(conn, mv)?;
        if targets.is_empty() {
            continue;
        }
        let native = targets
            .iter()
            .any(|t| t == "any" || chain.contains(&t.to_lowercase()));
        if native {
            continue;
        }
        let bridged_by = targets.iter().find_map(|t| {
            provides
                .get(&format!("loader:{}", t.to_lowercase()))
                .and_then(|by| by.iter().next().cloned())
        });
        let row = LoaderMismatch {
            filename: a.filename.clone(),
            artifact_loaders: targets,
            pack_loader: cfg.loader.name.clone(),
            bridged_by: bridged_by.clone(),
        };
        if bridged_by.is_some() {
            loader_bridged.push(row);
        } else {
            loader_mismatch.push(row);
        }
    }
    loader_mismatch.sort_by(|a, b| a.filename.cmp(&b.filename));
    loader_bridged.sort_by(|a, b| a.filename.cmp(&b.filename));

    let overlaps: Vec<CapabilityOverlap> = provides
        .into_iter()
        .filter(|(_, fns)| fns.len() >= 2)
        .map(|(capability, fns)| CapabilityOverlap {
            capability,
            mods: fns.into_iter().collect(),
        })
        .collect();

    // Classification advisories over the placed mods: what is not a mod at
    // all, what the classifier left undecided, where the sources disagree, and
    // which mods are server-side (shipped opted out).
    let mut coremods: Vec<String> = non_mods;
    let mut unclassified: Vec<String> = Vec::new();
    let mut side_disagreements: Vec<SideDisagreement> = Vec::new();
    let mut server_side: Vec<String> = Vec::new();
    for p in &present {
        let Some(c) = class_of.get(&p.mod_id) else {
            continue;
        };
        if c.is_non_mod() {
            coremods.push(p.filename.clone());
            continue;
        }
        if let Some((win, bc)) = c.side_disagreement() {
            side_disagreements.push(SideDisagreement {
                filename: p.filename.clone(),
                modrinth_side: win.as_str().to_string(),
                bytecode_side: bc.as_str().to_string(),
            });
        }
        if c.side == Some(SideClass::Server) {
            server_side.push(p.filename.clone());
        }
        if c.policy.is_none() {
            unclassified.push(p.filename.clone());
        }
    }
    coremods.sort();
    unclassified.sort();
    server_side.sort();
    side_disagreements.sort_by(|x, y| x.filename.cmp(&y.filename));
    let forced_client_attempts: Vec<ForcedClientEdge> = forced_client
        .into_values()
        .map(|mut f| {
            f.needed_by.sort();
            f.needed_by.dedup();
            f
        })
        .collect();

    let mut missing: Vec<MissingDep> = missing
        .into_values()
        .map(|mut d| {
            d.needed_by.sort();
            d
        })
        .collect();
    missing.sort_by(|x, y| x.target.cmp(&y.target));
    conflicts.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    optional_conflicts.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    version_issues.sort_by(|x, y| (&x.filename, &x.target).cmp(&(&y.filename, &y.target)));
    unresolved.sort();
    unresolved.dedup();

    // #145: a required mixin whose target the pack's own copy of the host lost.
    // Only judged where the pack itself owns the class's package: everything
    // else is the game's, or a mod that is simply absent, which is a different
    // finding with its own name.
    let by_mod: HashMap<i64, &Present> = present.iter().map(|p| (p.mod_id, p)).collect();
    let mut mixin_gaps: Vec<MixinGap> = Vec::new();
    for p in &present {
        let Some(mv) = p.mod_version_id else { continue };
        for (config, needed) in queries::mixin_needs(conn, mv)? {
            if crate::authoring::bytecode::is_platform(&needed) {
                continue;
            }
            let Some(prefix) = crate::authoring::bytecode::package_prefix(&needed) else {
                continue;
            };
            let Some(owner_mod) = queries::owner_mod_for_prefix(conn, &prefix)? else {
                continue; // nobody owns it, or several do -- not answerable
            };
            if owner_mod == p.mod_id {
                continue; // its own class
            }
            let Some(owner) = by_mod.get(&owner_mod) else {
                continue; // the owner is not in this pack
            };
            let Some(owner_mv) = owner.mod_version_id else {
                continue; // never read: cannot answer
            };
            if queries::artifact_has_class(conn, owner_mv, &needed)? == Some(false) {
                mixin_gaps.push(MixinGap {
                    filename: p.filename.clone(),
                    config: config.clone(),
                    needed,
                    owner: owner.filename.clone(),
                });
            }
        }
    }
    mixin_gaps.sort_by(|x, y| (&x.filename, &x.needed).cmp(&(&y.filename, &y.needed)));

    Ok(ResolveReport {
        declared_mods: cfg.mods.len(),
        // a Modrinth pin the mirror has not harvested is resolved too -- it is a
        // valid declaration the build will fetch, not an unidentified mod
        resolved_mods: present.len() + pinned_projects.len(),
        missing,
        conflicts,
        optional_conflicts,
        overlaps,
        version_issues,
        // the async pass fills this one; a pure resolve has not looked
        loader_version_issues: Vec::new(),
        loader_mismatch,
        loader_bridged,
        mixin_gaps,
        unresolved,
        version_windows_unchecked: unchecked,
        coremods,
        unclassified,
        side_disagreements,
        forced_client_attempts,
        server_side,
        suggestions: suggestions.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Display, LoaderSpec, PackConfig, PackTier, Visibility};
    use crate::registry::Registry;
    use crate::registry::upsert;

    const NOW: &str = "2026-07-15T00:00:00Z";

    fn declared(
        filename: &str,
        default_enabled: bool,
        source: SourceDecl,
    ) -> crate::domain::DeclaredMod {
        crate::domain::DeclaredMod {
            filename: filename.to_string(),
            default_enabled,
            source,
            display: None::<Display>,
            slug: None,
            pulled: false,
        }
    }

    fn cache(sha1: &str) -> SourceDecl {
        SourceDecl::SmrtCache {
            sha1: sha1.to_string(),
        }
    }

    fn config(mods: Vec<crate::domain::DeclaredMod>) -> PackConfig {
        PackConfig {
            pack_id: "test".into(),
            display_name: "Test".into(),
            tagline: String::new(),
            minecraft_version: "1.12.2".into(),
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2860".into(),
            },
            java_major: 8,
            version: None,
            tags: vec![],
            featured: false,
            mods,
            assets: vec![],
            auth: None,
            pack_meta: Default::default(),
            owner: 0,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    /// Register a mod (by modid) with one cached artifact; return nothing -- the
    /// pack refers to it by sha1.
    fn add_mod(r: &Registry, modid: &str, version: &str, sha1: &str) -> i64 {
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", modid)], NOW)?;
            upsert::upsert_mod_version(c, id, version, &["forge"], sha1, 10, None, None, NOW)?;
            Ok(id)
        })
        .unwrap()
    }

    fn relate(
        r: &Registry,
        from: i64,
        target: &str,
        range: Option<&str>,
        kind: RelKind,
        severity: Option<Severity>,
        src: crate::registry::model::Source,
    ) {
        r.with_conn_mut(|c| {
            // mod-level: these fixtures assert resolver behaviour, not scoping
            upsert::upsert_relation(c, from, None, target, range, kind, severity, src, NOW)?;
            Ok(())
        })
        .unwrap();
    }

    /// Register a mod whose artifact targets specific loaders.
    fn add_mod_for(r: &Registry, modid: &str, sha1: &str, loaders: &[&str]) -> i64 {
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", modid)], NOW)?;
            upsert::upsert_mod_version(c, id, "1.0", loaders, sha1, 10, None, None, NOW)?;
            Ok(id)
        })
        .unwrap()
    }

    // A pre-build check must recognise a valid Modrinth pin: litematica depends on
    // malilib by its Modrinth project, and the pack pins malilib from Modrinth but
    // the mirror has not harvested it. The build fetches it, so it is present -- the
    // dependency is satisfied and the pin is not an unidentified mod.
    #[test]
    fn a_modrinth_pin_satisfies_a_dependency_without_a_harvested_mod() {
        let r = Registry::open_in_memory().unwrap();
        let lite = add_mod(&r, "litematica", "1.0", "sha_lite");
        relate(
            &r,
            lite,
            "modrinth:malilib_proj",
            None,
            RelKind::Requires,
            None,
            crate::registry::model::Source::Modrinth,
        );
        let cfg = config(vec![
            declared("litematica.jar", true, cache("sha_lite")),
            declared(
                "malilib.jar",
                false,
                SourceDecl::Modrinth {
                    project_id: "malilib_proj".into(),
                    version_id: "v1".into(),
                },
            ),
        ]);
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert!(
            rep.missing.is_empty(),
            "the pin satisfies the modrinth dependency: {:?}",
            rep.missing
        );
        assert!(
            rep.unresolved.is_empty(),
            "a valid Modrinth pin is not unresolved: {:?}",
            rep.unresolved
        );
        assert_eq!(
            rep.resolved_mods, 2,
            "the harvested mod and the pin both count as resolved"
        );
    }

    // The dependency-fill plan: a hard dep on a present mod is an edge (for
    // display.requires); a hard dep nothing ships is missing (to be auto-pulled).
    #[test]
    fn dependency_fill_plan_splits_present_edges_from_missing() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "moda", "1", &"1".repeat(40));
        add_mod(&r, "modb", "1", &"2".repeat(40));
        relate(
            &r,
            a,
            "modb",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );
        relate(
            &r,
            a,
            "modc",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );
        let cfg = config(vec![
            declared("a.jar", true, cache(&"1".repeat(40))),
            declared("b.jar", true, cache(&"2".repeat(40))),
        ]);
        let plan = r.with_conn(|c| dependency_fill_plan(c, &cfg)).unwrap();
        assert_eq!(
            plan.missing
                .iter()
                .map(|t| t.selector.as_str())
                .collect::<Vec<_>>(),
            vec!["modc"],
            "modc is not in the pack"
        );
        assert_eq!(
            plan.requires,
            vec![("a.jar".to_string(), "b.jar".to_string())],
            "a.jar -> b.jar is a present-mod edge"
        );
    }

    // #50: a fabric jar in a forge pack will not load, and nothing in the pack says
    // otherwise -- that is a fact worth flagging.
    #[test]
    fn foreign_loader_artifact_without_a_bridge_is_flagged() {
        let r = Registry::open_in_memory().unwrap();
        add_mod_for(&r, "fab", "sha_fab", &["fabric"]);
        let cfg = config(vec![declared("fab.jar", true, cache("sha_fab"))]); // config() is forge
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();

        assert_eq!(rep.loader_mismatch.len(), 1);
        assert_eq!(rep.loader_mismatch[0].filename, "fab.jar");
        assert_eq!(rep.loader_mismatch[0].artifact_loaders, vec!["fabric"]);
        assert!(rep.loader_bridged.is_empty());
    }

    // A fork runs its parent's artifacts by construction, so a forge jar on a
    // cleanroom pack is not a mismatch at all (#37) -- and neither is an `any` jar.
    #[test]
    fn a_fork_runs_its_parents_artifacts_and_any_suits_everything() {
        let r = Registry::open_in_memory().unwrap();
        add_mod_for(&r, "fj", "sha_forge", &["forge"]);
        add_mod_for(&r, "tw", "sha_any", &["any"]);
        let mut cfg = config(vec![
            declared("forge.jar", true, cache("sha_forge")),
            declared("tweak.jar", true, cache("sha_any")),
        ]);
        cfg.loader.name = "cleanroom".into();
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert!(
            rep.loader_mismatch.is_empty(),
            "cleanroom inherits forge, and `any` suits any loader"
        );
    }

    // lwjgl3ify is a Forge patcher: a Forge jar (UniMixins, Angelica, any 1.7.10
    // mod) on an lwjgl3ify pack runs natively and must not read as mismatched.
    #[test]
    fn lwjgl3ify_runs_forge_artifacts_natively() {
        let r = Registry::open_in_memory().unwrap();
        add_mod_for(&r, "um", "sha_unimixins", &["forge"]);
        let mut cfg = config(vec![declared(
            "unimixins.jar",
            true,
            cache("sha_unimixins"),
        )]);
        cfg.loader.name = "lwjgl3ify".into();
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert!(
            rep.loader_mismatch.is_empty(),
            "lwjgl3ify inherits forge; a forge mod is native, not a mismatch"
        );
    }

    // A connector present in the pack carries the mod: not a finding, just a fact
    // worth listing -- pull the connector and every one of them goes at once.
    #[test]
    fn a_bridge_carries_a_foreign_artifact_and_is_not_a_finding() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        add_mod_for(&r, "fab", "sha_fab", &["fabric"]);
        let conn_mod = add_mod_for(&r, "connector", "sha_conn", &["forge"]);
        // the connector declares that it carries fabric's runtime
        relate(
            &r,
            conn_mod,
            "loader:fabric",
            None,
            RelKind::Provides,
            None,
            Source::Authored,
        );
        let cfg = config(vec![
            declared("fab.jar", true, cache("sha_fab")),
            declared("connector.jar", true, cache("sha_conn")),
        ]);
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();

        assert!(
            rep.loader_mismatch.is_empty(),
            "a connector carries it: not a problem"
        );
        assert_eq!(rep.loader_bridged.len(), 1, "still worth listing");
        assert_eq!(rep.loader_bridged[0].filename, "fab.jar");
        assert_eq!(
            rep.loader_bridged[0].bridged_by.as_deref(),
            Some("connector.jar")
        );
    }

    // Zoomify is fabric-only and rides Sinytra Connector on a neoforge pack. Its
    // Modrinth metadata declares Fabric API; what answers that on this side of
    // the bridge is Forgified Fabric API, a different project with a different
    // modid. Nothing in the graph joined the two, so the dependency read as unmet
    // and the pre-publish gate refused a pack that had been running for weeks.
    #[test]
    fn fabric_api_is_answered_by_the_forgified_port_across_a_bridge() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let fab = add_mod_for(&r, "zoomify", "sha_zoom", &["fabric"]);
        relate(
            &r,
            fab,
            "modrinth:P7dR8mSH",
            None,
            RelKind::Requires,
            None,
            Source::Modrinth,
        );
        let conn_mod = add_mod_for(&r, "connector", "sha_conn", &["forge"]);
        relate(
            &r,
            conn_mod,
            "loader:fabric",
            None,
            RelKind::Provides,
            None,
            Source::Authored,
        );
        add_mod_for(&r, "fabric_api", "sha_ffapi", &["forge"]);
        let cfg = config(vec![
            declared("Zoomify.jar", true, cache("sha_zoom")),
            declared("connector.jar", true, cache("sha_conn")),
            declared("forgified-fabric-api.jar", true, cache("sha_ffapi")),
        ]);

        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert!(
            rep.missing.is_empty(),
            "the forgified port answers it: {:?}",
            rep.missing
        );

        // and the fill plan does not offer to pull a dependency already answered
        let plan = r.with_conn(|c| dependency_fill_plan(c, &cfg)).unwrap();
        assert!(
            !plan
                .missing
                .iter()
                .any(|t| t.selector == "modrinth:P7dR8mSH"),
            "nothing to pull: {:?}",
            plan.missing.iter().map(|t| &t.selector).collect::<Vec<_>>()
        );
    }

    // The equivalence is not blanket. With nothing on this side of the bridge to
    // answer it, Fabric API is a dependency the pack genuinely does not ship, and
    // both legs must keep saying so -- otherwise the fix trades a false block for
    // a silent one.
    #[test]
    fn fabric_api_stays_missing_when_nothing_answers_it() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let fab = add_mod_for(&r, "zoomify", "sha_zoom", &["fabric"]);
        relate(
            &r,
            fab,
            "modrinth:P7dR8mSH",
            None,
            RelKind::Requires,
            None,
            Source::Modrinth,
        );
        let conn_mod = add_mod_for(&r, "connector", "sha_conn", &["forge"]);
        relate(
            &r,
            conn_mod,
            "loader:fabric",
            None,
            RelKind::Provides,
            None,
            Source::Authored,
        );
        let cfg = config(vec![
            declared("Zoomify.jar", true, cache("sha_zoom")),
            declared("connector.jar", true, cache("sha_conn")),
        ]);

        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert_eq!(rep.missing.len(), 1, "{:?}", rep.missing);
        assert_eq!(rep.missing[0].target, "modrinth:P7dR8mSH");

        let plan = r.with_conn(|c| dependency_fill_plan(c, &cfg)).unwrap();
        assert!(
            plan.missing
                .iter()
                .any(|t| t.selector == "modrinth:P7dR8mSH"),
            "still a pull candidate: {:?}",
            plan.missing.iter().map(|t| &t.selector).collect::<Vec<_>>()
        );
    }

    // The pack graph is the same shape as the registry graph, but read against the
    // pack: a target the pack ships resolves, and one it does not stays dangling.
    // That is what lets the panel read a dangling `requires` as a missing dependency
    // and a `conflicts` between two present mods as a live one.
    #[test]
    fn pack_graph_resolves_only_what_the_pack_ships() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "aaa", "1.0", "sha_a");
        let b = add_mod(&r, "bbb", "1.0", "sha_b");
        add_mod(&r, "ccc", "1.0", "sha_c"); // in the registry, but not in this pack
        relate(&r, a, "bbb", None, RelKind::Requires, None, Source::JarMeta); // satisfied here
        relate(&r, a, "ccc", None, RelKind::Requires, None, Source::JarMeta); // not shipped
        relate(
            &r,
            a,
            "bbb",
            None,
            RelKind::Conflicts,
            Some(Severity::Hard),
            Source::Authored,
        ); // live conflict
        let _ = b;

        let cfg = config(vec![
            declared("a.jar", true, cache("sha_a")),
            declared("b.jar", true, cache("sha_b")),
        ]);
        let g = r.with_conn(|c| pack_graph(c, &cfg)).unwrap();

        // every declared mod is a node, whether or not it has relations
        assert_eq!(g.nodes.len(), 2);

        let find = |target: &str, kind: &str| {
            g.edges
                .iter()
                .find(|e| e.target == target && e.kind == kind)
                .unwrap_or_else(|| panic!("no {kind} edge to {target}"))
        };
        assert!(
            find("bbb", "requires").to_mod_id.is_some(),
            "a requirement the pack ships resolves"
        );
        assert!(
            find("ccc", "requires").to_mod_id.is_none(),
            "a requirement the pack does not ship dangles -- that is the missing dep"
        );
        assert!(
            find("bbb", "conflicts").to_mod_id.is_some(),
            "a conflict between two shipped mods is live"
        );
    }

    // An artifact's own facts, not its mod's other versions (#48), decide what the
    // pack graph draws: shipping the old jar must not pull the new jar's dependency.
    #[test]
    fn pack_graph_follows_the_shipped_artifact() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let (m, old, new) = r
            .with_conn_mut(|c| {
                let m = upsert::upsert_mod_by_alias(c, &[("modid", "mmm")], NOW)?;
                let old = upsert::upsert_mod_version(
                    c,
                    m,
                    "1.0",
                    &["forge"],
                    "sha_old",
                    1,
                    None,
                    None,
                    NOW,
                )?;
                let new = upsert::upsert_mod_version(
                    c,
                    m,
                    "2.0",
                    &["forge"],
                    "sha_new",
                    1,
                    None,
                    None,
                    NOW,
                )?;
                Ok((m, old, new))
            })
            .unwrap();
        add_mod(&r, "oldlib", "1.0", "sha_oldlib");
        add_mod(&r, "newlib", "1.0", "sha_newlib");
        r.with_conn_mut(|c| {
            upsert::upsert_relation(
                c,
                m,
                Some(old),
                "oldlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                m,
                Some(new),
                "newlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();

        // the pack ships the OLD jar
        let cfg = config(vec![declared("m.jar", true, cache("sha_old"))]);
        let g = r.with_conn(|c| pack_graph(c, &cfg)).unwrap();
        let targets: Vec<&str> = g.edges.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(
            targets,
            vec!["oldlib"],
            "the shipped artifact's dependency only -- the sibling version's is not this pack's"
        );
    }

    #[test]
    fn missing_hard_dep_is_flagged_when_target_absent() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let stuff = add_mod(&r, "ae2stuff", "0.7.0", &"a".repeat(40));
        add_mod(&r, "appliedenergistics2", "0.44", &"b".repeat(40));
        relate(
            &r,
            stuff,
            "appliedenergistics2",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );

        // AE2 present -> satisfied
        let ok = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![
                        declared("ae2stuff.jar", true, cache(&"a".repeat(40))),
                        declared("ae2.jar", true, cache(&"b".repeat(40))),
                    ]),
                )
            })
            .unwrap();
        assert!(ok.missing.is_empty(), "AE2 present: {:?}", ok.missing);
        assert_eq!(ok.resolved_mods, 2);

        // AE2 removed -> missing
        let bad = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![declared("ae2stuff.jar", true, cache(&"a".repeat(40)))]),
                )
            })
            .unwrap();
        assert_eq!(bad.missing.len(), 1);
        assert_eq!(bad.missing[0].target, "appliedenergistics2");
        assert_eq!(bad.missing[0].needed_by, vec!["ae2stuff.jar"]);
    }

    // A capability the loader ships natively answers a dependency with no jar for
    // it in the pack: Cleanroom loads mixins, so a MixinBooter dependency is met
    // with MixinBooter removed -- neither reported missing nor auto-pulled. The
    // same dependency on a plain Forge pack, where nothing provides it, is missing.
    #[test]
    fn a_loader_provided_capability_satisfies_a_dependency() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let culling = add_mod(&r, "entityculling", "1.0", &"a".repeat(40));
        // depends on MixinBooter by its Modrinth project, as the real edge does
        relate(
            &r,
            culling,
            "modrinth:G1ckZuWK",
            None,
            RelKind::Requires,
            None,
            Source::Modrinth,
        );
        let pack = || {
            config(vec![declared(
                "entityculling.jar",
                true,
                cache(&"a".repeat(40)),
            )])
        };

        // Forge: nothing provides it -> missing
        let mut forge = pack();
        forge.loader.name = "forge".into();
        let rep = r.with_conn(|c| resolve_pack(c, &forge)).unwrap();
        assert_eq!(rep.missing.len(), 1, "Forge does not bundle mixins");
        assert_eq!(rep.missing[0].target, "modrinth:G1ckZuWK");

        // Cleanroom: the loader answers it -> clean, and the fill plan does not
        // list it as an auto-pull candidate either
        let mut cleanroom = pack();
        cleanroom.loader.name = "cleanroom".into();
        let rep = r.with_conn(|c| resolve_pack(c, &cleanroom)).unwrap();
        assert!(
            rep.missing.is_empty(),
            "Cleanroom loads mixins natively: {:?}",
            rep.missing
        );
        let plan = r
            .with_conn(|c| dependency_fill_plan(c, &cleanroom))
            .unwrap();
        assert!(
            plan.missing
                .iter()
                .all(|t| t.selector != "modrinth:G1ckZuWK"),
            "a loader-provided capability is not auto-pulled"
        );
    }

    #[test]
    fn authored_optional_suppresses_inferred_requires() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let jei = add_mod(&r, "somemod", "1.0", &"c".repeat(40));
        // inferred says requires jei; authored says it's only optional
        relate(
            &r,
            jei,
            "jei",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );
        relate(
            &r,
            jei,
            "jei",
            None,
            RelKind::OptionalDep,
            None,
            Source::Authored,
        );

        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![declared("somemod.jar", true, cache(&"c".repeat(40)))]),
                )
            })
            .unwrap();
        assert!(
            rep.missing.is_empty(),
            "authored optional wins: {:?}",
            rep.missing
        );
    }

    #[test]
    fn active_conflict_between_two_present_mods() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "moda", "1.0", &"d".repeat(40));
        add_mod(&r, "modb", "1.0", &"e".repeat(40));
        relate(
            &r,
            a,
            "modb",
            None,
            RelKind::Conflicts,
            Some(Severity::Hard),
            Source::Authored,
        );

        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![
                        declared("a.jar", true, cache(&"d".repeat(40))),
                        declared("b.jar", true, cache(&"e".repeat(40))),
                    ]),
                )
            })
            .unwrap();
        assert_eq!(rep.conflicts.len(), 1);
        assert_eq!(rep.conflicts[0].a, "a.jar");
        assert_eq!(rep.conflicts[0].b, "b.jar");
        assert!(rep.conflicts[0].hard);
    }

    #[test]
    fn conflict_with_an_opted_out_optional_is_advisory() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "moda", "1.0", &"d".repeat(40));
        add_mod(&r, "modb", "1.0", &"e".repeat(40));
        relate(
            &r,
            a,
            "modb",
            None,
            RelKind::Conflicts,
            Some(Severity::Hard),
            Source::Authored,
        );

        // b.jar is an optional the pack ships disabled: the conflict only bites if
        // the user turns it on, so it is advisory, not a blocking conflict (#9).
        let b_off = crate::domain::DeclaredMod {
            filename: "b.jar".into(),
            default_enabled: false,
            source: cache(&"e".repeat(40)),
            display: None::<Display>,
            slug: None,
            pulled: false,
        };
        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![declared("a.jar", true, cache(&"d".repeat(40))), b_off]),
                )
            })
            .unwrap();
        assert!(
            rep.conflicts.is_empty(),
            "an opted-out optional is not a blocking conflict"
        );
        assert_eq!(rep.optional_conflicts.len(), 1);
        assert_eq!(rep.optional_conflicts[0].b, "b.jar");
    }

    #[test]
    fn a_loader_dependency_is_not_missing() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "moda", "1.0", &"d".repeat(40));
        // moda requires the loader, spelled the Forge way, with a version window
        relate(
            &r,
            a,
            "MinecraftForge",
            None,
            RelKind::Requires,
            None,
            Source::JarMeta,
        );

        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![declared("a.jar", true, cache(&"d".repeat(40)))]),
                )
            })
            .unwrap();
        assert!(
            rep.missing.is_empty(),
            "a dependency on the loader is not a missing mod (#10)"
        );
    }

    #[test]
    fn capability_overlap_and_provides_satisfaction() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let ctm = add_mod(&r, "ctm", "1.0", &"1".repeat(40));
        let fusion = add_mod(&r, "fusion", "1.0", &"2".repeat(40));
        let user = add_mod(&r, "needsctm", "1.0", &"3".repeat(40));
        relate(
            &r,
            ctm,
            "connected_textures",
            None,
            RelKind::Provides,
            None,
            Source::Authored,
        );
        relate(
            &r,
            fusion,
            "connected_textures",
            None,
            RelKind::Provides,
            None,
            Source::Authored,
        );
        // a mod requiring the capability is satisfied by a provider, not "missing"
        relate(
            &r,
            user,
            "connected_textures",
            None,
            RelKind::Requires,
            None,
            Source::Authored,
        );

        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![
                        declared("ctm.jar", true, cache(&"1".repeat(40))),
                        declared("fusion.jar", true, cache(&"2".repeat(40))),
                        declared("user.jar", true, cache(&"3".repeat(40))),
                    ]),
                )
            })
            .unwrap();
        assert_eq!(rep.overlaps.len(), 1);
        assert_eq!(rep.overlaps[0].capability, "connected_textures");
        assert_eq!(rep.overlaps[0].mods, vec!["ctm.jar", "fusion.jar"]);
        assert!(
            rep.missing.is_empty(),
            "capability satisfies requires: {:?}",
            rep.missing
        );
    }

    #[test]
    fn version_window_flagged_only_when_comparable() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let dep = add_mod(&r, "usesnewlib", "1.0", &"9".repeat(40));
        add_mod(&r, "somelib", "1.0.0", &"0".repeat(40));
        relate(
            &r,
            dep,
            "somelib",
            Some("[2.0,)"),
            RelKind::Requires,
            None,
            Source::JarMeta,
        );

        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![
                        declared("uses.jar", true, cache(&"9".repeat(40))),
                        declared("lib.jar", true, cache(&"0".repeat(40))),
                    ]),
                )
            })
            .unwrap();
        assert_eq!(rep.version_issues.len(), 1);
        assert_eq!(rep.version_issues[0].filename, "lib.jar");
        assert_eq!(rep.version_issues[0].present_version, "1.0.0");
        assert_eq!(rep.version_issues[0].required_range, "[2.0,)");
    }

    // The client/server guard: an inferred hard edge into a client-side mod is
    // soft everywhere -- no requires edge for the fill plan, no missing report,
    // no pull -- while a DECLARED hard edge stays and is reported as a
    // forced-client attempt.
    #[test]
    fn inferred_hard_edge_into_a_client_mod_is_soft() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let chisel = add_mod(&r, "chisel", "1.0", "sha_chisel");
        add_mod(&r, "ctm", "1.0", "sha_ctm");
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(
                c,
                "sha_ctm",
                "mod",
                Some("client"),
                None,
                None,
            )?;
            Ok(())
        })
        .unwrap();
        relate(
            &r,
            chisel,
            "ctm",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );

        // ctm present: no requires edge lands in the fill plan
        let both = config(vec![
            declared("chisel.jar", true, cache("sha_chisel")),
            declared("ctm.jar", true, cache("sha_ctm")),
        ]);
        let plan = r.with_conn(|c| dependency_fill_plan(c, &both)).unwrap();
        assert!(
            plan.requires.is_empty(),
            "an inferred edge into a client mod records no requires edge: {:?}",
            plan.requires
        );
        let rep = r.with_conn(|c| resolve_pack(c, &both)).unwrap();
        assert!(
            rep.forced_client_attempts.is_empty(),
            "inferred edges never report forced"
        );

        // ctm absent: not missing, not pulled
        let alone = config(vec![declared("chisel.jar", true, cache("sha_chisel"))]);
        let plan = r.with_conn(|c| dependency_fill_plan(c, &alone)).unwrap();
        assert!(
            plan.missing.is_empty(),
            "a client mod is never pulled: {:?}",
            plan.missing
        );
        let rep = r.with_conn(|c| resolve_pack(c, &alone)).unwrap();
        assert!(rep.missing.is_empty(), "not reported missing either");
    }

    // A bytecode-inferred edge onto a both-side content mod (a mod that merely
    // implements Mekanism's energy API) is the class-granularity false positive
    // that dirtied every self-hosted pack: it must not pull the content mod.
    #[test]
    fn inferred_hard_edge_into_a_both_content_mod_is_soft() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let ec = add_mod(&r, "energycontrol", "1.0", "sha_ec");
        add_mod(&r, "mekanism", "1.0", "sha_mek");
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(c, "sha_mek", "mod", Some("both"), None, None)?;
            Ok(())
        })
        .unwrap();
        relate(
            &r,
            ec,
            "mekanism",
            None,
            RelKind::Requires,
            None,
            Source::Inferred,
        );

        // mekanism absent: not missing, not pulled
        let alone = config(vec![declared("EnergyControl.jar", true, cache("sha_ec"))]);
        let plan = r.with_conn(|c| dependency_fill_plan(c, &alone)).unwrap();
        assert!(
            plan.missing.is_empty(),
            "an inferred edge onto a both-side content mod never pulls it: {:?}",
            plan.missing
        );
    }

    #[test]
    fn declared_hard_edge_into_a_client_mod_is_reported() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "moda", "1.0", "sha_a");
        add_mod(&r, "clientmod", "1.0", "sha_cl");
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(
                c,
                "sha_cl",
                "mod",
                Some("client"),
                Some("tolerant"),
                None,
            )?;
            Ok(())
        })
        .unwrap();
        relate(
            &r,
            a,
            "clientmod",
            None,
            RelKind::Requires,
            None,
            Source::JarMeta,
        );
        let cfg = config(vec![
            declared("a.jar", true, cache("sha_a")),
            declared("client.jar", true, cache("sha_cl")),
        ]);
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert_eq!(rep.forced_client_attempts.len(), 1);
        let f = &rep.forced_client_attempts[0];
        assert_eq!(f.filename, "client.jar");
        assert_eq!(f.needed_by, vec!["a.jar"]);
        assert_eq!(f.source, "jar-meta");
        // the declared edge is NOT silently dropped from the plan
        let plan = r.with_conn(|c| dependency_fill_plan(c, &cfg)).unwrap();
        assert_eq!(
            plan.requires,
            vec![("a.jar".to_string(), "client.jar".to_string())]
        );

        // a client mod requiring another client mod is a normal co-toggled
        // chain, not a forced-client attempt
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(
                c,
                "sha_a",
                "mod",
                Some("client"),
                Some("tolerant"),
                Some("high"),
            )?;
            Ok(())
        })
        .unwrap();
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert!(
            rep.forced_client_attempts.is_empty(),
            "client -> client is not reported: {:?}",
            rep.forced_client_attempts
        );
    }

    // The classification advisories: an identity-less coremod jar, an
    // unclassified mod, a server-side mod, and a Modrinth-vs-bytecode side
    // disagreement all surface in their own report sections.
    #[test]
    fn report_carries_the_classification_advisories() {
        let r = Registry::open_in_memory().unwrap();
        // a bare ASM library: jar_class row, no registry identity
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(
                c,
                &"a".repeat(40),
                "library",
                None,
                None,
                None,
            )?;
            Ok(())
        })
        .unwrap();
        // an unclassified placed mod (no jar_class, no env flags)
        add_mod(&r, "mystery", "1.0", "sha_my");
        // a server-side mod
        add_mod(&r, "servux", "1.0", "sha_srv");
        // a mod whose Modrinth flags say client while the bytecode says both
        let dis = add_mod(&r, "disputed", "1.0", "sha_dis");
        r.with_conn_mut(|c| {
            crate::registry::upsert::set_jar_class(
                c,
                "sha_srv",
                "mod",
                Some("server"),
                Some("tolerant"),
                None,
            )?;
            crate::registry::upsert::set_mod_env_flags(
                c,
                dis,
                Some("required"),
                Some("unsupported"),
                NOW,
            )?;
            crate::registry::upsert::set_jar_class(
                c,
                "sha_dis",
                "mod",
                Some("both"),
                Some("must_match"),
                None,
            )?;
            Ok(())
        })
        .unwrap();
        let cfg = config(vec![
            declared("ChickenASM.jar", true, cache(&"a".repeat(40))),
            declared("mystery.jar", true, cache("sha_my")),
            declared("servux.jar", true, cache("sha_srv")),
            declared("disputed.jar", true, cache("sha_dis")),
        ]);
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert_eq!(rep.coremods, vec!["ChickenASM.jar"]);
        assert!(
            rep.unresolved.is_empty(),
            "a classified non-mod jar is not unresolved: {:?}",
            rep.unresolved
        );
        assert!(rep.unclassified.contains(&"mystery.jar".to_string()));
        assert_eq!(rep.server_side, vec!["servux.jar"]);
        assert_eq!(rep.side_disagreements.len(), 1);
        let d = &rep.side_disagreements[0];
        assert_eq!(
            (
                d.filename.as_str(),
                d.modrinth_side.as_str(),
                d.bytecode_side.as_str()
            ),
            ("disputed.jar", "client", "both")
        );
    }

    // An external dependency (out of both ecosystems) reports missing with the
    // external reason, so the curator sees it is not a resolver bug.
    #[test]
    fn external_missing_dep_carries_its_reason() {
        use crate::registry::model::Source;
        let r = Registry::open_in_memory().unwrap();
        let a = add_mod(&r, "hostmod", "1.0", "sha_h");
        relate(
            &r,
            a,
            "external:OptiFine.jar",
            None,
            RelKind::Requires,
            None,
            Source::Modrinth,
        );
        let cfg = config(vec![declared("host.jar", true, cache("sha_h"))]);
        let rep = r.with_conn(|c| resolve_pack(c, &cfg)).unwrap();
        assert_eq!(rep.missing.len(), 1);
        assert_eq!(rep.missing[0].target, "external:OptiFine.jar");
        assert_eq!(rep.missing[0].reason.as_deref(), Some("external"));
    }

    #[test]
    fn unresolved_jar_is_listed_not_judged() {
        let r = Registry::open_in_memory().unwrap();
        // sha1 never harvested
        let rep = r
            .with_conn(|c| {
                resolve_pack(
                    c,
                    &config(vec![declared("ghost.jar", true, cache(&"f".repeat(40)))]),
                )
            })
            .unwrap();
        assert_eq!(rep.resolved_mods, 0);
        assert_eq!(rep.unresolved, vec!["ghost.jar"]);
        assert!(rep.missing.is_empty());
    }
}
