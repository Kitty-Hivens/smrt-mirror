//! Registry endpoints (`/v1/registry/*`): trigger a harvest and read the
//! mod-identity index (which packs use a mod, all versions, orphans, loader
//! eligibility). Auth-gated like the rest of the write API -- these expose pack
//! composition.
//!
//! Phase 1 runs the harvest synchronously and returns the report; a single
//! operator tolerates the few-second wait. Streaming it as a job is a Phase 2
//! nicety, not needed here.

use super::page::PageQuery;
use super::{ApiError, audit};
use crate::accounts::Identity;
use crate::authoring::HarvestStatus;
use crate::authoring::{JarDiff, diff_jars, reconstruct_config};
use crate::domain::DeclaredAsset;
use crate::registry::model::{
    BuildModRow, BuildSummary, EligibleArtifact, GraphData, GraphSlice, ModUse,
    ModrinthProjectName, OrphanJar, RegistryStats, RelKind, ReleaseRow, Severity, UnassignedJar,
    VersionRow,
};
use crate::registry::{authored, queries};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri};
use axum::middleware::from_fn_with_state;
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

pub fn router(state: AppState) -> Router {
    operator_routes(state.clone())
        .merge(member_routes(state.clone()))
        .merge(debug_routes(state))
}

/// Read-only registry views any signed-in user may see. Authoring stays gated in
/// `operator_routes` / `debug_routes`; these only read, and expose no pack
/// composition -- the faceted mod list and a mod's releases are mod identity and
/// its files, the graph is mods and their relations, and "which pack" is a
/// separate query none of them run. It is the same data the mod page already shows
/// publicly, so a member browsing the registry to build a community pack has no
/// reason to be denied it.
fn member_routes(state: AppState) -> Router {
    Router::new()
        .route("/v1/registry/mods", get(get_mods))
        .route(
            "/v1/registry/mod-releases/{mod_id}",
            get(get_releases_by_id),
        )
        .route("/v1/registry/graph", get(get_graph))
        .route("/v1/registry/graph/slices", get(get_graph_slices))
        .route("/v1/registry/modrinth-names", get(get_modrinth_names))
        .layer(from_fn_with_state(
            state.clone(),
            super::auth::require_session,
        ))
        .with_state(state)
}

/// Reads, harvest, backup, and cosmetic authoring (mod rename) -- none of these
/// move the compatibility graph, so the admin role is enough.
fn operator_routes(state: AppState) -> Router {
    Router::new()
        .route("/v1/registry/harvest", post(post_harvest))
        .route("/v1/registry/harvest/status", get(get_harvest_status))
        .route("/v1/registry/stats", get(get_stats))
        .route("/v1/registry/orphans", get(get_orphans))
        .route("/v1/registry/eligible", get(get_eligible))
        // registry browser: versions by surrogate id, builds (the faceted mod list
        // and a mod's releases are read-only and open to members -- see below)
        .route(
            "/v1/registry/mod-versions/{mod_id}",
            get(get_versions_by_id),
        )
        .route("/v1/registry/builds", get(get_builds))
        .route(
            "/v1/registry/builds/{pack_id}/{pack_version}",
            get(get_build_mods),
        )
        .route(
            "/v1/registry/builds/{pack_id}/{pack_version}/assets",
            get(get_build_assets),
        )
        .route(
            "/v1/registry/mods/{alias_source}/{external_key}",
            get(get_mod_versions),
        )
        .route(
            "/v1/registry/mods/{alias_source}/{external_key}/uses",
            get(get_mod_uses),
        )
        .route("/v1/registry/backup", post(post_backup))
        // needs-identity door: jars with no identity yet (listing only; assigning
        // one asserts compat facts, so the write side is debug-gated below)
        .route("/v1/registry/unassigned", get(get_unassigned))
        // repackage (tamper) diff: what a self-hosted jar changed vs its genuine
        // Modrinth counterpart. Read-only.
        .route(
            "/v1/registry/files/{sha1}/repack-diff",
            get(get_repack_diff),
        )
        // cosmetic: canonical name / slug, nothing the resolver reads
        .route("/v1/registry/mod-meta/{mod_id}", put(put_mod_rename))
        // Naming a cached jar (its mod, version, channel, loaders/mc). It does
        // feed the derivation, but it is the operator's routine job of putting a
        // name to a jar the mirror already holds, not a surgical override of it,
        // so it sits at admin rather than the debug rung (#13).
        .route("/v1/registry/files/{sha1}/identity", put(put_file_identity))
        .layer(from_fn_with_state(state.clone(), super::auth::require_auth))
        .with_state(state)
}

/// Compat-affecting authoring (#39): rewriting a release's version number,
/// merging two mods into one identity, or a dependency/conflict fact. These
/// surgically move the derivation graph the eligibility + resolver ride on --
/// unlike naming a jar (now admin, #13) -- so they sit above the admin token on
/// the debug rung and are audited.
fn debug_routes(state: AppState) -> Router {
    Router::new()
        .route("/v1/registry/conflicts", post(post_conflict))
        .route("/v1/registry/releases/{release_id}", put(put_release_edit))
        .route("/v1/registry/merge", post(post_merge))
        .route("/v1/registry/relations", post(post_relation))
        // classification override: moves the side/policy the required
        // derivation and the client invariant ride on
        .route("/v1/registry/files/{sha1}/class", put(put_file_class))
        .layer(from_fn_with_state(
            state.clone(),
            super::auth::require_debug,
        ))
        .with_state(state)
}

/// Run a blocking registry write off the async runtime, and announce that the
/// index moved.
///
/// Every registry mutation passes through here, so this is where "a registry
/// view is now stale" is said once rather than remembered at each of the nine
/// call sites. `what` names the mutation, so a listener showing one mod can
/// tell a rename from a merge. Announced only on success: a write that failed
/// changed nothing to refetch.
async fn run_write<T>(
    state: &AppState,
    what: &'static str,
    f: impl FnOnce(&crate::registry::Registry) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    let reg = state.registry.clone();
    let res = tokio::task::spawn_blocking(move || f(&reg))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("registry write task: {e}")))?;
    let out = res?;
    state.events.registry(what);
    Ok(out)
}

/// Run a registry read off the async runtime (rusqlite is blocking).
async fn run_query<T, F>(state: &AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> anyhow::Result<T> + Send + 'static,
{
    let reg = state.registry.clone();
    let res = tokio::task::spawn_blocking(move || reg.with_conn(f))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("registry query task: {e}")))?;
    Ok(res?)
}

/// Which of `shas` the mirror actually holds in its local cache. The registry
/// indexes manifest-only (e.g. Modrinth-sourced) artifacts too, so "in the
/// registry" does not mean "on disk" -- the panel needs this to pick a
/// `smrt_cache` vs a `modrinth` source per artifact.
///
/// Asked about the rows being answered rather than about the cache as a whole:
/// a page of twenty artifacts costs twenty lookups, not a walk of every jar the
/// mirror has ever kept.
async fn cache_shas(
    state: &AppState,
    shas: impl IntoIterator<Item = String>,
) -> std::collections::HashSet<String> {
    state.storage.cached_among(shas).await
}

/// Force an immediate harvest and return at once with the harvester's status. The
/// run happens on the background worker (skipping its debounce), not inline, so a
/// harvest that outlasts the gateway timeout no longer 504s the operator's request
/// -- the result is read from [`get_harvest_status`]. Coalesced with a concurrent
/// auto-harvest: a force during a run leaves a wake for one follow-up pass.
async fn post_harvest(State(state): State<AppState>) -> (StatusCode, Json<HarvestStatus>) {
    state.harvest.force();
    (StatusCode::ACCEPTED, Json(state.harvest.status()))
}

/// The background harvester's current state: whether a run is in flight, and the
/// report or error of the last completed one. Polled after a forced harvest.
async fn get_harvest_status(State(state): State<AppState>) -> Json<HarvestStatus> {
    Json(state.harvest.status())
}

async fn get_stats(State(state): State<AppState>) -> Result<Json<RegistryStats>, ApiError> {
    Ok(Json(run_query(&state, queries::stats).await?))
}

async fn get_orphans(State(state): State<AppState>) -> Result<Json<Vec<OrphanJar>>, ApiError> {
    Ok(Json(run_query(&state, queries::orphan_jars).await?))
}

/// The dependency/conflict graph (nodes + edges) for the graph view.
#[derive(Deserialize)]
struct GraphQuery {
    mc: Option<String>,
    loader: Option<String>,
}

/// The relation graph, narrowed to one (Minecraft version, loader) world when
/// asked (#49). Unnarrowed it is the union across every version of every mod,
/// which is only readable while the registry holds a single world -- the panel
/// picks a slice and this answers for it. The loader match is fork-aware, so a
/// cleanroom slice sees the forge artifacts it can actually run.
async fn get_graph(
    State(state): State<AppState>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<GraphData>, ApiError> {
    // empty query params arrive as Some("") -- treat blank as "do not narrow"
    let blank = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (mc, loader) = (blank(&q.mc), blank(&q.loader));
    Ok(Json(
        run_query(&state, move |c| {
            queries::graph_for_slice(c, mc.as_deref(), loader.as_deref())
        })
        .await?,
    ))
}

/// The (Minecraft version, loader) worlds the registry holds, busiest first, so
/// the panel offers real choices and opens on one that has something in it.
async fn get_graph_slices(
    State(state): State<AppState>,
) -> Result<Json<Vec<GraphSlice>>, ApiError> {
    Ok(Json(run_query(&state, queries::graph_slices).await?))
}

#[derive(Deserialize)]
struct NamesQuery {
    /// comma-separated Modrinth project ids
    ids: Option<String>,
}

/// Resolve Modrinth project ids to display names for the graph's external
/// `modrinth:<id>` leaves. Cache-first: cached rows return at once, and the ids
/// still missing are fetched from Modrinth in one batch, stored, and merged.
/// Best-effort -- if Modrinth is unreachable the resolved names are returned and
/// the rest stay as their ids on the client. Member-gated, so the id list is not
/// an open Modrinth proxy; capped regardless.
async fn get_modrinth_names(
    State(state): State<AppState>,
    Query(q): Query<NamesQuery>,
) -> Result<Json<Vec<ModrinthProjectName>>, ApiError> {
    let mut ids: Vec<String> = q
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids.truncate(256);
    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let want = ids.clone();
    let mut resolved = run_query(&state, move |c| queries::cached_modrinth_names(c, &want)).await?;

    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !resolved.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        match state.modrinth.projects_by_ids(&missing).await {
            Ok(projects) => {
                let fresh: Vec<ModrinthProjectName> = projects
                    .into_values()
                    .map(|p| ModrinthProjectName {
                        id: p.id,
                        title: p.title,
                        slug: Some(p.slug),
                    })
                    .collect();
                let store = fresh.clone();
                run_write(&state, "modrinth-names", move |reg| {
                    reg.cache_modrinth_names(&store)
                })
                .await?;
                for n in fresh {
                    resolved.insert(n.id.clone(), n);
                }
            }
            // names are cosmetic; a Modrinth outage must not fail the graph
            Err(e) => tracing::warn!("modrinth name resolve failed: {e:#}"),
        }
    }

    let names = ids
        .into_iter()
        .filter_map(|id| resolved.remove(&id))
        .collect();
    Ok(Json(names))
}

#[derive(Deserialize)]
struct EligibleQuery {
    loader: String,
}

async fn get_eligible(
    State(state): State<AppState>,
    Query(q): Query<EligibleQuery>,
) -> Result<Json<Vec<EligibleArtifact>>, ApiError> {
    Ok(Json(
        run_query(&state, move |c| queries::eligible_for_loader(c, &q.loader)).await?,
    ))
}

#[derive(Deserialize)]
struct ModsQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default)]
    mc: Option<String>,
}

async fn get_mods(
    State(state): State<AppState>,
    Query(q): Query<ModsQuery>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    // empty query params arrive as Some("") -- treat blank as "no filter"
    let blank = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (q_, loader_, mc_) = (blank(&q.q), blank(&q.loader), blank(&q.mc));
    let after = page
        .cursor()
        .and_then(|parts| parts.first()?.parse::<i64>().ok());
    let take = page.probe();
    let rows = run_query(&state, move |c| {
        queries::list_mods(
            c,
            q_.as_deref(),
            loader_.as_deref(),
            mc_.as_deref(),
            after,
            take,
        )
    })
    .await?;
    Ok(page.answer(rows, &uri, |m| vec![m.mod_id.to_string()]))
}

async fn get_versions_by_id(
    State(state): State<AppState>,
    Path(mod_id): Path<i64>,
) -> Result<Json<Vec<VersionRow>>, ApiError> {
    let mut rows = run_query(&state, move |c| queries::versions_of_mod_by_id(c, mod_id)).await?;
    let shas: Vec<String> = rows.iter().map(|r| r.sha1.clone()).collect();
    let cached = cache_shas(&state, shas).await;
    for r in &mut rows {
        r.cached = cached.contains(&r.sha1);
    }
    // Only offer artifacts the panel can actually install: bytes in the local
    // cache (re-add as smrt_cache) or a Modrinth identity (re-add from Modrinth).
    // A harvested version whose jar was removed (not cached, no Modrinth id) is a
    // historical row kept for build provenance -- it has nothing to download, so
    // listing it just shows a dead duplicate. A null filename can't be shown or
    // installed either, so drop those too.
    rows.retain(|r| r.filename.is_some() && (r.cached || r.modrinth_version_id.is_some()));
    Ok(Json(rows))
}

// The mod's files grouped by release (version node) for the management view.
// Unlike get_versions_by_id (the picker, which hides non-installable artifacts),
// this shows every file so the operator can manage the full set; `cached` is set
// per file against the live cache.
async fn get_releases_by_id(
    State(state): State<AppState>,
    Path(mod_id): Path<i64>,
) -> Result<Json<Vec<ReleaseRow>>, ApiError> {
    let mut releases =
        run_query(&state, move |c| queries::releases_of_mod_by_id(c, mod_id)).await?;
    let shas: Vec<String> = releases
        .iter()
        .flat_map(|rel| &rel.files)
        .map(|f| f.sha1.clone())
        .collect();
    let cached = cache_shas(&state, shas).await;
    for rel in &mut releases {
        for f in &mut rel.files {
            f.cached = cached.contains(&f.sha1);
        }
    }
    Ok(Json(releases))
}

async fn get_builds(State(state): State<AppState>) -> Result<Json<Vec<BuildSummary>>, ApiError> {
    Ok(Json(run_query(&state, queries::list_builds).await?))
}

async fn get_build_mods(
    State(state): State<AppState>,
    Path((pack_id, pack_version)): Path<(String, String)>,
) -> Result<Json<Vec<BuildModRow>>, ApiError> {
    let mut rows = run_query(&state, move |c| {
        queries::build_mods(c, &pack_id, &pack_version)
    })
    .await?;
    let shas: Vec<String> = rows.iter().map(|r| r.sha1.clone()).collect();
    let cached = cache_shas(&state, shas).await;
    for r in &mut rows {
        r.cached = cached.contains(&r.sha1);
    }
    Ok(Json(rows))
}

// A build's assets, as re-addable declarations. The registry indexes mods, not
// assets, so this reads them from the build's published manifest (via the same
// manifest->config reconstruction the revert path uses) -- each asset keeps its
// exact source (Modrinth / static / cache).
async fn get_build_assets(
    State(state): State<AppState>,
    Path((pack_id, pack_version)): Path<(String, String)>,
) -> Result<Json<Vec<DeclaredAsset>>, ApiError> {
    let manifest = state
        .storage
        .load_manifest_version(&pack_id, &pack_version)
        .await?;
    let summary = state.storage.load_pack_summary(&pack_id).await?;
    Ok(Json(reconstruct_config(&manifest, &summary).assets))
}

async fn get_mod_versions(
    State(state): State<AppState>,
    Path((alias_source, external_key)): Path<(String, String)>,
) -> Result<Json<Vec<VersionRow>>, ApiError> {
    let mut rows = run_query(&state, move |c| {
        queries::versions_of_mod(c, &alias_source, &external_key)
    })
    .await?;
    let shas: Vec<String> = rows.iter().map(|r| r.sha1.clone()).collect();
    let cached = cache_shas(&state, shas).await;
    for r in &mut rows {
        r.cached = cached.contains(&r.sha1);
    }
    Ok(Json(rows))
}

async fn get_mod_uses(
    State(state): State<AppState>,
    Path((alias_source, external_key)): Path<(String, String)>,
) -> Result<Json<Vec<ModUse>>, ApiError> {
    Ok(Json(
        run_query(&state, move |c| {
            queries::packs_using_mod(c, &alias_source, &external_key)
        })
        .await?,
    ))
}

// ── authored moderation (Phase 2) ────────────────────────────────────────────

#[derive(Deserialize)]
struct ConflictBody {
    a_modid: String,
    b_modid: String,
    #[serde(default)]
    remove: bool,
}

async fn post_conflict(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(b): Json<ConflictBody>,
) -> Result<StatusCode, ApiError> {
    let (a_modid, b_modid, remove) = (b.a_modid.clone(), b.b_modid.clone(), b.remove);
    run_write(&state, "conflict", move |reg| {
        reg.set_conflict(&b.a_modid, &b.b_modid, b.remove)
    })
    .await?;
    let action = if remove {
        "registry.conflict.remove"
    } else {
        "registry.conflict.add"
    };
    audit(&state, &identity, action, Some(&a_modid), Some(&b_modid)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MergeBody {
    from_mod_id: i64,
    into_mod_id: i64,
}

/// Merge two mod identities into one (the surviving `into_mod_id`). Compat-
/// affecting registry surgery, so debug-gated and audited.
async fn post_merge(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(b): Json<MergeBody>,
) -> Result<StatusCode, ApiError> {
    let (from, into) = (b.from_mod_id, b.into_mod_id);
    run_write(&state, "merge", move |reg| reg.merge_mods(from, into))
        .await
        .map_err(|e| match e {
            ApiError::Internal(inner) => ApiError::BadRequest(inner.to_string()),
            other => other,
        })?;
    audit(
        &state,
        &identity,
        "registry.merge",
        Some(&into.to_string()),
        Some(&format!("from {from}")),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RelationBody {
    from_mod_id: i64,
    target_modid: String,
    kind: String,
    /// `hard` | `soft`, for a conflict. Absent reads as hard: an incompatibility
    /// whose intensity nobody stated is the one to surface (#129). Ignored for
    /// every other kind, which carries no intensity.
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    remove: bool,
}

/// Author or remove a single graph edge -- the node editor's write. An added
/// edge is `authored` (it survives re-harvest and outranks a bytecode inference
/// for the same target); a remove drops only the authored row. Compat-affecting,
/// so debug-gated and audited.
async fn post_relation(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(b): Json<RelationBody>,
) -> Result<StatusCode, ApiError> {
    let kind = RelKind::parse(&b.kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown relation kind {:?}", b.kind)))?;
    let severity = match b.severity.as_deref() {
        None => None,
        Some(s) => Some(
            Severity::parse(s)
                .ok_or_else(|| ApiError::BadRequest(format!("unknown severity {s:?}")))?,
        ),
    };
    let (from, target, remove) = (b.from_mod_id, b.target_modid.clone(), b.remove);
    run_write(&state, "relation", move |reg| {
        reg.author_relation(b.from_mod_id, &b.target_modid, kind, severity, b.remove)
    })
    .await?;
    let action = if remove {
        "registry.relation.remove"
    } else {
        "registry.relation.add"
    };
    audit(
        &state,
        &identity,
        action,
        Some(&from.to_string()),
        Some(&format!("{} {target}", kind.as_str())),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct BackupResult {
    path: String,
}

async fn post_backup(State(state): State<AppState>) -> Result<Json<BackupResult>, ApiError> {
    let dir = state.config.storage_dir.join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::Internal(e.into()))?;
    let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let dest = dir.join(format!("registry-{stamp}.db"));
    let target = dest.clone();
    run_write(&state, "backup", move |reg| reg.backup_into(&target)).await?;
    Ok(Json(BackupResult {
        path: dest.to_string_lossy().into_owned(),
    }))
}

// Compare a self-hosted jar against its genuine Modrinth counterpart and report
// what changed, class files apart from resource churn. Reads the repackaged bytes
// from the local cache, fetches the genuine file from Modrinth, diffs by entry
// CRC off the runtime. Read-only.
async fn get_repack_diff(
    State(state): State<AppState>,
    Path(sha1): Path<String>,
) -> Result<Json<JarDiff>, ApiError> {
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("sha1 must be 40 hex chars".into()));
    }
    let sha_q = sha1.clone();
    let (project, version_id) = run_query(&state, move |c| queries::repack_counterpart(c, &sha_q))
        .await?
        .ok_or_else(|| ApiError::BadRequest("no Modrinth counterpart to diff against".into()))?;

    let path = state.storage.cache_jar_path(&sha1[..2], &sha1)?;
    let repack = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::NotFound)?;

    let version = state
        .modrinth
        .project_version(&project, &version_id)
        .await
        .map_err(ApiError::Internal)?;
    let url = version
        .primary_file()
        .map(|f| f.url.clone())
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("modrinth version carries no file")))?;
    let genuine = state
        .modrinth
        .fetch_bytes(&url)
        .await
        .map_err(ApiError::Internal)?;

    let diff = tokio::task::spawn_blocking(move || diff_jars(&repack, &genuine))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("diff task: {e}")))?
        .map_err(ApiError::Internal)?;
    Ok(Json(diff))
}

// ── authored identity door (Phase B) ─────────────────────────────────────────

// Jars in the cache with no registry identity yet: the live cache inventory minus
// every sha1 the registry has a row for. These are what harvest could not
// identify (aliasless jars it drops) -- the "needs identity" bucket.
async fn get_unassigned(
    State(state): State<AppState>,
) -> Result<Json<Vec<UnassignedJar>>, ApiError> {
    let inv = state.storage.list_cache_inventory().await?;
    let known = run_query(&state, queries::all_mod_version_shas).await?;
    // What the harvest read out of each of them (#123). Without this the answer
    // is a hash and a file size, which is a pile rather than a queue: naming a
    // jar meant downloading and opening it, so nobody did.
    let read = run_query(&state, queries::jar_readouts).await?;
    let out = inv
        .into_iter()
        .filter(|e| !known.contains(&e.sha1))
        .map(|e| {
            let r = read.get(&e.sha1).cloned().unwrap_or_default();
            UnassignedJar {
                size_bytes: e.size_bytes as i64,
                modid: r.modid,
                name: r.name,
                version: r.version,
                loaders: r.loaders,
                mc: r.mc,
                filename: r.filename,
                kind: r.kind,
                sha1: e.sha1,
            }
        })
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
struct IdentityBody {
    /// Assign to an existing mod (its surrogate id) ...
    #[serde(default)]
    mod_id: Option<i64>,
    /// ... or create a new authored mod with this display name. Exactly one of
    /// `mod_id` / `mod_name` is required.
    #[serde(default)]
    mod_name: Option<String>,
    version_number: String,
    channel: String,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    mc_versions: Vec<String>,
    /// Optional display filename for a loose cache jar (it carries none itself).
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Serialize)]
struct AuthoredFile {
    mod_version_id: i64,
}

// Set a cached jar's identity (mod + release + loader/mc) as an operator
// decision. The sha1 must be a jar the mirror holds -- its size is read from the
// cache, and the write is `source='authored'` so a re-harvest never reverts it.
async fn put_file_identity(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(sha1): Path<String>,
    Json(body): Json<IdentityBody>,
) -> Result<Json<AuthoredFile>, ApiError> {
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("sha1 must be 40 hex chars".into()));
    }
    // Reject operator input up front so a bad body is a 400, not a 500 from the
    // writer's backstop bail. The writer still validates (defence in depth).
    if body.version_number.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "version_number must not be empty".into(),
        ));
    }
    if !authored::CHANNELS.contains(&body.channel.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "channel must be one of {:?}",
            authored::CHANNELS
        )));
    }
    let has_mod = body.mod_id.is_some()
        || body
            .mod_name
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty());
    if !has_mod {
        return Err(ApiError::BadRequest(
            "provide mod_id (existing mod) or a non-empty mod_name (new mod)".into(),
        ));
    }
    let size = state
        .storage
        .cache_jar_size(&sha1)
        .await
        .map(|n| n as i64)
        .ok_or_else(|| {
            ApiError::BadRequest(
                "sha1 is not in the mirror cache; only cached jars can be authored".into(),
            )
        })?;

    // captured before `body` moves into the write closure
    let audit_target = sha1.clone();
    let audit_detail = format!("{} {}", body.version_number.trim(), body.channel);
    let mv_id = run_write(&state, "identity", move |reg| {
        let mod_ref = match (body.mod_id, body.mod_name.as_deref().map(str::trim)) {
            (Some(id), _) => authored::ModRef::Existing(id),
            (None, Some(name)) if !name.is_empty() => authored::ModRef::New { name },
            _ => anyhow::bail!("provide mod_id (existing mod) or a non-empty mod_name (new mod)"),
        };
        let fid = authored::FileIdentity {
            sha1: &sha1,
            size_bytes: size,
            filename: body.filename.as_deref(),
            mod_ref,
            version_number: body.version_number.trim(),
            channel: &body.channel,
            loaders: &body.loaders,
            mc_versions: &body.mc_versions,
        };
        reg.author_file(&fid)
    })
    .await?;
    audit(
        &state,
        &identity,
        "registry.file.identity",
        Some(&audit_target),
        Some(&audit_detail),
    )
    .await;
    Ok(Json(AuthoredFile {
        mod_version_id: mv_id,
    }))
}

#[derive(Deserialize)]
struct FileClassBody {
    /// mod | coremod | library. Required unless removing.
    #[serde(default)]
    kind: Option<String>,
    /// client | server | both.
    #[serde(default)]
    side: Option<String>,
    /// must_match | tolerant.
    #[serde(default)]
    match_policy: Option<String>,
    /// Drop the authored override; the next harvest re-derives.
    #[serde(default)]
    remove: bool,
}

/// Set (or clear) a jar's authored classification -- the operator's answer for
/// a jar the classifier left undecided or graded with a low-confidence
/// heuristic. Refused for a Modrinth-identified mod (its environment flags are
/// authoritative). Compat-affecting (feeds required derivation + the client
/// invariant), so debug-gated and audited.
async fn put_file_class(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(sha1): Path<String>,
    Json(b): Json<FileClassBody>,
) -> Result<StatusCode, ApiError> {
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("sha1 must be 40 hex chars".into()));
    }
    let kind = match (&b.kind, b.remove) {
        (Some(k), _) => k.clone(),
        (None, true) => String::new(), // unused on remove
        (None, false) => {
            return Err(ApiError::BadRequest(
                "kind is required (mod | coremod | library)".into(),
            ));
        }
    };
    let (sha_w, side, policy, remove) = (
        sha1.clone(),
        b.side.clone(),
        b.match_policy.clone(),
        b.remove,
    );
    run_write(&state, "class", move |reg| {
        reg.author_jar_class(&sha_w, &kind, side.as_deref(), policy.as_deref(), remove)
    })
    .await
    .map_err(|e| match e {
        ApiError::Internal(inner) => ApiError::BadRequest(inner.to_string()),
        other => other,
    })?;
    let action = if b.remove {
        "registry.file.class.remove"
    } else {
        "registry.file.class"
    };
    let detail = format!(
        "{} {} {}",
        b.kind.as_deref().unwrap_or("-"),
        b.side.as_deref().unwrap_or("-"),
        b.match_policy.as_deref().unwrap_or("-"),
    );
    audit(&state, &identity, action, Some(&sha1), Some(&detail)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RenameModBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
}

async fn put_mod_rename(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(mod_id): Path<i64>,
    Json(b): Json<RenameModBody>,
) -> Result<StatusCode, ApiError> {
    let detail = [b.name.clone(), b.slug.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    run_write(&state, "rename", move |reg| {
        reg.rename_mod(mod_id, b.name.as_deref(), b.slug.as_deref())
    })
    .await?;
    // Audited like every other authored registry write. It was the one that
    // was not, and it is not a small one: the name and slug are what the panel
    // and a merge read a mod by.
    audit(
        &state,
        &identity,
        "registry.mod.rename",
        Some(&mod_id.to_string()),
        Some(&detail),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct EditReleaseBody {
    #[serde(default)]
    version_number: Option<String>,
    #[serde(default)]
    channel: Option<String>,
}

async fn put_release_edit(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(release_id): Path<i64>,
    Json(b): Json<EditReleaseBody>,
) -> Result<StatusCode, ApiError> {
    if let Some(ch) = b.channel.as_deref()
        && !authored::CHANNELS.contains(&ch)
    {
        return Err(ApiError::BadRequest(format!(
            "channel must be one of {:?}",
            authored::CHANNELS
        )));
    }
    let detail = [b.version_number.clone(), b.channel.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    run_write(&state, "release", move |reg| {
        reg.edit_release(
            release_id,
            b.version_number.as_deref(),
            b.channel.as_deref(),
        )
    })
    .await?;
    audit(
        &state,
        &identity,
        "registry.release.edit",
        Some(&release_id.to_string()),
        Some(&detail),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
