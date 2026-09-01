use super::ApiError;
use super::admin::CommitDiff;
use super::page::{PageQuery, next_link};
use crate::accounts::{Notification, PackLevel, Thread, ThreadView};
use crate::authoring::jar_icon;
use crate::domain::*;
use crate::registry::model::{FileDetail, ModDetail};
use crate::registry::queries;
use crate::state::AppState;
use crate::storage::IconRead;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use std::collections::{HashMap, HashSet};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/packs", get(list_packs))
        .route("/v1/packs/{pack_id}", get(get_pack_summary))
        // Static segments win over dynamic in axum 0.8, so order does not
        // matter for /manifest/versions vs /manifest/{version}, but keeping
        // the more specific routes first matches the spec ordering.
        .route("/v1/packs/{pack_id}/manifest", get(get_latest_manifest))
        .route(
            "/v1/packs/{pack_id}/manifest/versions",
            get(list_manifest_versions),
        )
        .route(
            "/v1/packs/{pack_id}/manifest/{version}",
            get(get_manifest_version),
        )
        .route("/v1/packs/{pack_id}/diff", get(get_pack_diff))
        .route("/v1/packs/{pack_id}/threads", get(public_threads))
        .route("/v1/threads/{id}", get(public_thread))
        .route("/v1/threads/{id}/diff", get(public_thread_diff))
        .route("/v1/feed.atom", get(personal_feed))
        .route(
            "/v1/packs/{pack_id}/static/{*rel_path}",
            get(get_pack_static),
        )
        .route("/v1/servers", get(list_servers))
        .route("/v1/servers/{server_id}", get(get_server))
        .route("/v1/featured", get(get_featured))
        .route("/v1/cache/{prefix}/{filename}", get(get_cache_jar))
        .route("/v1/cache/icon/{sha1}", get(get_cache_icon))
        .route("/v1/modrinth/icon/{project_id}", get(get_project_icon))
        .route("/v1/cache/inventory", get(get_cache_inventory))
        .route("/v1/community", get(list_community))
        .route("/v1/mods/{key}", get(get_mod_detail))
        .route("/v1/files/{sha1}", get(get_file_detail))
        .route("/v1/users/{uid}/avatar", get(get_user_avatar))
        .with_state(state)
}

// ── /v1/files/:sha1 ────────────────────────────────────────────────────────

/// Hash-first artifact lookup, the Modrinth `version_file/{hash}` analog: a
/// launcher holding a jar (or a manifest entry) resolves what it is without
/// walking the mod page. Same public read model as `/v1/mods/{key}`.
#[utoipa::path(
    get,
    path = "/v1/files/{sha1}",
    tag = "public",
    params(("sha1" = String, Path, description = "Full 40-char sha1 of the artifact")),
    responses(
        (status = 200, description = "The file, its release, and the owning mod", body = FileDetail),
        (status = 404, description = "No registered artifact with that hash")
    )
)]
pub(crate) async fn get_file_detail(
    State(state): State<AppState>,
    Path(sha1): Path<String>,
) -> Result<Json<FileDetail>, ApiError> {
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("sha1 must be 40 hex chars".into()));
    }
    let reg = state.registry.clone();
    let detail =
        tokio::task::spawn_blocking(move || reg.with_conn(|c| queries::file_detail(c, &sha1)))
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("file detail task: {e}")))??;
    let Some(mut detail) = detail else {
        return Err(ApiError::NotFound);
    };
    detail.file.cached = state.storage.has_cache_jar(&detail.file.sha1).await;
    Ok(Json(detail))
}

// ── /v1/mods/:id ─────────────────────────────────────────────────────────────

/// The public read model behind a single mod's page: identity, releases (files),
/// the relations that touch it, and the packs that ship it. Read-only and
/// unauthenticated -- mod metadata is not sensitive, and the mirror already
/// serves the jars themselves. `used_by` is narrowed to official + published
/// packs so a guest cannot learn a draft's name from it; file `cached` flags are
/// set here against the live cache. Operators reuse this view and reach the gated
/// edit/diff endpoints separately.
///
/// `key` is a numeric mod id (the graph and registry navigate by id), a
/// `sha1:<hash>` artifact reference (a pack's mod list has the jar's sha1, not
/// the mod id), or a Modrinth-style slug -- all resolve to the same page.
#[utoipa::path(
    get,
    path = "/v1/mods/{key}",
    tag = "public",
    params(("key" = String, Path,
        description = "Numeric mod id, `sha1:<hash>` of any of the mod's artifacts, or the mod's slug")),
    responses(
        (status = 200, description = "The mod's public page model", body = ModDetail),
        (status = 404, description = "No mod resolves from the key")
    )
)]
pub(crate) async fn get_mod_detail(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<ModDetail>, ApiError> {
    let reg = state.registry.clone();
    let detail = tokio::task::spawn_blocking(move || {
        reg.with_conn(|c| {
            let mod_id = if let Ok(id) = key.parse::<i64>() {
                Some(id)
            } else if let Some(sha1) = key.strip_prefix("sha1:") {
                queries::mod_id_for_sha1(c, sha1)?
            } else {
                queries::mod_id_for_slug(c, &key)?
            };
            match mod_id {
                Some(id) => queries::mod_detail(c, id),
                None => Ok(None),
            }
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("mod detail task: {e}")))??;
    let Some(mut detail) = detail else {
        return Err(ApiError::NotFound);
    };

    // asked about this mod's own files, not about the whole cache
    let shas: Vec<String> = detail
        .releases
        .iter()
        .flat_map(|rel| &rel.files)
        .map(|f| f.sha1.clone())
        .collect();
    let cached = state.storage.cached_among(shas).await;
    for rel in &mut detail.releases {
        for f in &mut rel.files {
            f.cached = cached.contains(&f.sha1);
        }
    }

    // never surface a draft/unlisted/community pack name to a guest through the
    // "used by" list -- keep it to the same set the launcher's catalog exposes.
    let public_packs: HashSet<String> = state
        .storage
        .list_pack_summaries()
        .await?
        .into_iter()
        .filter(|p| p.tier == PackTier::Official && p.visibility == Visibility::Published)
        .map(|p| p.pack_id)
        .collect();
    detail.used_by.retain(|u| public_packs.contains(&u.pack_id));

    Ok(Json(detail))
}

// ── /v1/health ─────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "public",
    responses((status = 200, description = "Mirror is up", body = Health))
)]
pub(crate) async fn health() -> Json<Health> {
    Json(Health {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        // calendar version stamped by build.rs -- moves with the code
        version: env!("SMRT_BUILD_VERSION"),
    })
}

// ── /v1/packs ──────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/v1/packs",
    tag = "public",
    responses((status = 200, description = "The launcher catalog: official, published packs", body = PackListing))
)]
pub(crate) async fn list_packs(
    State(state): State<AppState>,
) -> Result<Json<PackListing>, ApiError> {
    // The launcher's catalog is official + published only; drafts, unlisted, and
    // community packs are reached through other surfaces, never this listing.
    let mut packs: Vec<PackSummary> = state
        .storage
        .list_pack_summaries()
        .await?
        .into_iter()
        .filter(|p| p.tier == PackTier::Official && p.visibility == Visibility::Published)
        .collect();
    for p in &mut packs {
        enrich_latest_build(&state, p).await;
    }
    Ok(Json(PackListing {
        schema_version: SCHEMA_VERSION,
        generated_at: now_rfc3339(),
        packs,
    }))
}

/// Stamp `latest_built_at` / `latest_channel` onto a summary from the latest
/// manifest's header. Read-time derivation, so the values can never drift from
/// the manifest they describe; a pack without a readable build simply keeps
/// both fields absent.
pub(super) async fn enrich_latest_build(state: &AppState, summary: &mut PackSummary) {
    if let Ok(Some(info)) = state.storage.latest_build_info(&summary.pack_id).await {
        summary.latest_built_at = Some(info.date_published);
        summary.latest_channel = Some(info.version_type);
    }
}

/// Published community packs for the site's Community view -- browseable here but
/// never part of the launcher's official `/v1/packs` catalog. Each carries the
/// owner's login (resolved from the uid) for the byline.
#[utoipa::path(
    get,
    path = "/v1/community",
    tag = "public",
    responses((status = 200, description = "Published community packs with owner byline", body = Vec<CommunityPack>))
)]
pub(crate) async fn list_community(
    State(state): State<AppState>,
) -> Result<Json<Vec<CommunityPack>>, ApiError> {
    let summaries: Vec<PackSummary> = state
        .storage
        .list_pack_summaries()
        .await?
        .into_iter()
        .filter(|p| p.tier == PackTier::Community && p.visibility == Visibility::Published)
        .collect();

    let acc = state.accounts.clone();
    let users = tokio::task::spawn_blocking(move || acc.list_users())
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("community users task: {e}")))??;
    let logins: HashMap<i64, String> = users.into_iter().map(|u| (u.github_uid, u.login)).collect();

    let mut out: Vec<CommunityPack> = summaries
        .into_iter()
        .map(|s| {
            let owner_login = logins
                .get(&s.owner)
                .cloned()
                .unwrap_or_else(|| format!("uid {}", s.owner));
            CommunityPack {
                summary: s,
                owner_login,
            }
        })
        .collect();
    for p in &mut out {
        enrich_latest_build(&state, &mut p.summary).await;
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}",
    tag = "public",
    params(("pack_id" = String, Path, description = "Pack identifier")),
    responses((status = 200, body = PackSummary), (status = 404, description = "No such pack"))
)]
pub(crate) async fn get_pack_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<PackSummary>, ApiError> {
    let mut summary = state.storage.load_pack_summary(&pack_id).await?;
    gate_summary(&state, &headers, &pack_id, &summary).await?;
    enrich_latest_build(&state, &mut summary).await;
    Ok(Json(summary))
}

#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/manifest",
    tag = "public",
    params(("pack_id" = String, Path, description = "Pack identifier")),
    responses(
        (status = 200, description = "The pack's latest manifest", body = PackManifest),
        (status = 404, description = "No such pack, or no build yet")
    )
)]
pub(crate) async fn get_latest_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<PackManifest>, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    Ok(Json(state.storage.load_latest_manifest(&pack_id).await?))
}

#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/manifest/{version}",
    tag = "public",
    params(
        ("pack_id" = String, Path, description = "Pack identifier"),
        ("version" = String, Path, description = "Exact pack version label")
    ),
    responses(
        (status = 200, description = "The manifest of that build", body = PackManifest),
        (status = 404, description = "No such pack or version")
    )
)]
pub(crate) async fn get_manifest_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((pack_id, version)): Path<(String, String)>,
) -> Result<Json<PackManifest>, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    Ok(Json(
        state
            .storage
            .load_manifest_version(&pack_id, &version)
            .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/manifest/versions",
    tag = "public",
    params(("pack_id" = String, Path, description = "Pack identifier")),
    responses(
        (status = 200,
         description = "Every retained build, newest first, plus the current latest",
         body = ManifestVersionsListing),
        (status = 404, description = "No such pack")
    )
)]
pub(crate) async fn list_manifest_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<ManifestVersionsListing>, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    let builds = state.storage.list_manifest_builds(&pack_id).await?;
    let latest = state.storage.latest_manifest_version(&pack_id).await?;
    Ok(Json(ManifestVersionsListing {
        schema_version: SCHEMA_VERSION,
        pack_id,
        latest,
        builds,
    }))
}

#[derive(serde::Deserialize)]
pub(crate) struct DiffParams {
    from: String,
    /// Defaults to the latest build.
    to: Option<String>,
}

/// The update-dialog endpoint: what actually changes between two builds --
/// loader/minecraft/java bumps, mods added/removed/updated (matched by stable
/// identity, so a re-pin that renames the jar reads as an update), install
/// default flips, asset changes. Version labels are enriched from the
/// registry where it knows the artifacts.
#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/diff",
    tag = "public",
    params(
        ("pack_id" = String, Path, description = "Pack identifier"),
        ("from" = String, Query, description = "Installed (older) version label"),
        ("to" = Option<String>, Query, description = "Target version label; defaults to latest")
    ),
    responses(
        (status = 200, description = "The structured change summary from -> to", body = PackDiff),
        (status = 404, description = "No such pack or version")
    )
)]
pub(crate) async fn get_pack_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    axum::extract::Query(p): axum::extract::Query<DiffParams>,
) -> Result<Json<PackDiff>, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    let from = state
        .storage
        .load_manifest_version(&pack_id, &p.from)
        .await?;
    let to = match &p.to {
        Some(v) => state.storage.load_manifest_version(&pack_id, v).await?,
        None => state.storage.load_latest_manifest(&pack_id).await?,
    };
    let mut diff = diff_manifests(&from, &to);

    // enrich with the registry's version labels where it knows the artifacts
    let shas: Vec<String> = diff
        .mods_updated
        .iter()
        .flat_map(|u| [u.sha1_from.clone(), u.sha1_to.clone()])
        .chain(diff.mods_added.iter().map(|_| String::new()))
        .filter(|s| !s.is_empty())
        .collect();
    if !shas.is_empty() {
        let reg = state.registry.clone();
        let labels: HashMap<String, String> = tokio::task::spawn_blocking(move || {
            reg.with_conn(|c| {
                let mut out = HashMap::new();
                for sha in &shas {
                    if let Some(v) = queries::version_label_for_sha1(c, sha)? {
                        out.insert(sha.clone(), v);
                    }
                }
                Ok(out)
            })
        })
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("diff label task: {e}")))??;
        for u in &mut diff.mods_updated {
            u.version_from = labels.get(&u.sha1_from).cloned();
            u.version_to = labels.get(&u.sha1_to).cloned();
        }
    }
    Ok(Json(diff))
}

#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/static/{rel_path}",
    tag = "public",
    params(
        ("pack_id" = String, Path, description = "Pack identifier"),
        ("rel_path" = String, Path, description = "Path under the pack's static tree")
    ),
    responses(
        (status = 200, description = "The static file bytes, content type by extension"),
        (status = 404, description = "No such pack or file")
    )
)]
pub(crate) async fn get_pack_static(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path((pack_id, rel_path)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    let path = state.storage.pack_static_path(&pack_id, &rel_path)?;
    if tokio::fs::metadata(&path).await.is_err() {
        return Err(ApiError::NotFound);
    }
    serve_file(&method, &headers, &path, content_type_for(&rel_path)).await
}

/// Draft packs are private -- readable only by their owner (or an admin), so a
/// work-in-progress pack does not leak by direct id. Unlisted and published stay
/// readable by id (unlisted is off-catalog but link-shareable). A denied draft
/// answers `NotFound`, not 403, so a private pack's existence stays unconfirmed.
async fn gate_summary(
    state: &AppState,
    headers: &HeaderMap,
    pack_id: &str,
    summary: &PackSummary,
) -> Result<(), ApiError> {
    if summary.visibility != Visibility::Draft {
        return Ok(());
    }
    match super::auth::optional_identity(state, headers).await {
        Some(id) if super::auth::may(state, &id, pack_id, PackLevel::View).await => Ok(()),
        _ => Err(ApiError::NotFound),
    }
}

async fn gate_pack_read(
    state: &AppState,
    headers: &HeaderMap,
    pack_id: &str,
) -> Result<(), ApiError> {
    let summary = state.storage.load_pack_summary(pack_id).await?;
    gate_summary(state, headers, pack_id, &summary).await
}

// ── discussions, in the open ────────────────────────────────────────────────

/// What is being said about a published pack: reports and proposals, with their
/// outcomes. Readable without a session, because a decision nobody can see is
/// indistinguishable from one nobody made -- an unpublished pack's discussion
/// stays with whoever can see the pack.
#[utoipa::path(
    get,
    path = "/v1/packs/{pack_id}/threads",
    tag = "public",
    params(
        ("pack_id" = String, Path, description = "Pack id"),
        ("kind" = Option<String>, Query, description = "`issue` | `proposal`; absent lists both"),
        ("all" = Option<bool>, Query, description = "Include settled threads"),
        ("limit" = Option<usize>, Query, description = "Page size; absent answers the whole list"),
        ("after" = Option<String>, Query, description = "Cursor from the previous page's `Link`")
    ),
    responses(
        (status = 200, description = "Reports and proposals on a published pack", body = [Thread]),
        (status = 404, description = "No such pack, or its draft is not yours to read")
    )
)]
pub(crate) async fn public_threads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Query(p): Query<PublicThreadParams>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    gate_pack_read(&state, &headers, &pack_id).await?;
    let acc = state.accounts.clone();
    let (pack, kind) = (pack_id.clone(), p.kind.clone());
    // The cursor is the last row's sort key, which for this listing is the pair
    // the order is by; a cursor that does not decode reads as the start.
    let after = page.cursor().and_then(|parts| match parts.as_slice() {
        [at, id] => Some((at.parse().ok()?, id.parse().ok()?)),
        _ => None,
    });
    let probe = page.probe();
    let rows = tokio::task::spawn_blocking(move || {
        acc.threads_for(&pack, kind.as_deref(), !p.all, after, probe)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    Ok(page.answer(rows, &uri, |t| {
        vec![t.created_at.to_string(), t.id.to_string()]
    }))
}

#[derive(serde::Deserialize)]
pub(crate) struct PublicThreadParams {
    kind: Option<String>,
    #[serde(default)]
    all: bool,
}

/// One discussion in full. The pack it belongs to decides who may read it, so a
/// draft's thread stays as private as the draft.
#[utoipa::path(
    get,
    path = "/v1/threads/{id}",
    tag = "public",
    params(
        ("id" = i64, Path, description = "Thread id"),
        ("limit" = Option<usize>, Query, description = "Comments per page; absent answers all of them"),
        ("after" = Option<String>, Query, description = "Cursor from the previous page's `Link`")
    ),
    responses(
        (status = 200, description = "One discussion in full", body = ThreadView),
        (status = 404, description = "No such thread, or its pack is not yours to read")
    )
)]
pub(crate) async fn public_thread(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let acc = state.accounts.clone();
    let thread = tokio::task::spawn_blocking(move || acc.thread(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    gate_pack_read(&state, &headers, &thread.pack_id).await?;
    let acc = state.accounts.clone();
    // What is paged here is the discussion, not the thread: a long argument
    // arrives a page at a time and the thread it is about rides with every page,
    // so a reader who follows the `Link` never holds half an answer.
    let after = page
        .cursor()
        .and_then(|parts| parts.first()?.parse::<i64>().ok());
    let probe = page.probe();
    let comments = tokio::task::spawn_blocking(move || acc.comments_on(id, after, probe))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    let (comments, next) = page.split(comments, |c| vec![c.id.to_string()]);
    let mut resp = Json(crate::accounts::ThreadView { thread, comments }).into_response();
    if let Some(value) = next_link(&uri, next.as_deref()) {
        resp.headers_mut().insert(header::LINK, value);
    }
    Ok(resp)
}

/// What a proposal would change, read against the target as it stands now --
/// not against the fork's parent. The review question is "what happens to this
/// pack if the offer is taken", and that answer moves as the pack moves.
///
/// As readable as the thread it belongs to, which is what makes a decision
/// checkable: "they turned this down" means nothing to a reader who cannot see
/// what was turned down. Offering a commit to a pack publishes that commit's
/// content to the pack's readers -- the offer is the act of publication, so a
/// fork that is not ready to be read is not ready to be proposed.
#[utoipa::path(
    get,
    path = "/v1/threads/{id}/diff",
    tag = "public",
    params(("id" = i64, Path, description = "Thread id of a proposal")),
    responses(
        (status = 200, description = "What taking the offer would change", body = CommitDiff),
        (status = 400, description = "That thread is a report and offers no state"),
        (status = 404, description = "No such thread, or its pack is not yours to read")
    )
)]
pub(crate) async fn public_thread_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<CommitDiff>, ApiError> {
    let acc = state.accounts.clone();
    let thread = tokio::task::spawn_blocking(move || acc.thread(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    gate_pack_read(&state, &headers, &thread.pack_id).await?;
    let (Some(source_pack), Some(source_commit)) = (&thread.source_pack, &thread.source_commit)
    else {
        return Err(ApiError::BadRequest("this thread offers no state".into()));
    };
    let offered = state
        .storage
        .load_commit_config(source_pack, source_commit)
        .await?;
    let current = state.storage.load_pack_config(&thread.pack_id).await?;
    Ok(Json(CommitDiff {
        from: Some(thread.pack_id.clone()),
        to: source_commit.clone(),
        changes: crate::authoring::diff_configs(&current, &offered),
    }))
}

// ── /v1/feed.atom ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct FeedParams {
    key: String,
}

/// Somebody's own notifications, as a feed.
///
/// The panel is where these are read, and the panel is not always open. A feed
/// is the way to be told without the mirror having to reach anybody: nothing
/// leaves this machine, no address of anybody's is stored, no third party is
/// asked to deliver, and the reader is whatever the person already uses.
///
/// It is on the public router because a feed reader has no session and cannot
/// be given one: the key in the address is what names the account. That makes
/// the address itself the secret -- it can be rotated, and rotating retires
/// whatever was handed out before. An unknown key is a `404`, the same answer as
/// a key that never existed, so the endpoint says nothing about which accounts
/// have feeds.
pub(crate) async fn personal_feed(
    State(state): State<AppState>,
    Query(p): Query<FeedParams>,
) -> Result<Response, ApiError> {
    let acc = state.accounts.clone();
    let key = p.key.clone();
    let uid = tokio::task::spawn_blocking(move || acc.uid_for_feed_key(&key))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("feed task: {e}")))??
        .ok_or(ApiError::NotFound)?;

    let acc = state.accounts.clone();
    let (rows, login) = tokio::task::spawn_blocking(move || {
        let rows = acc.notifications_for(uid, false, None, Some(50))?;
        let login = acc.identity_of(uid)?.map(|i| i.login);
        Ok::<_, anyhow::Error>((rows, login))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("feed task: {e}")))??;

    let base = state.config.mirror_base.trim_end_matches('/');
    let body = atom_feed(base, login.as_deref().unwrap_or("you"), uid, &rows);
    Ok((
        [
            (header::CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
            // a feed read by a reader on a timer is not something to cache
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        body,
    )
        .into_response())
}

/// The feed document. Written out rather than generated by a library: it is
/// forty lines of a format that has not moved in fifteen years, and every value
/// in it comes from somewhere a person typed, so the escaping is the part that
/// matters.
fn atom_feed(base: &str, login: &str, uid: i64, rows: &[Notification]) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str(&format!("  <title>smrt: {}</title>\n", xml(login)));
    out.push_str(&format!("  <id>tag:smrt,2026:notifications:{uid}</id>\n"));
    out.push_str(&format!(
        "  <link rel=\"alternate\" href=\"{}/profile\"/>\n",
        xml(base)
    ));
    let newest = rows.first().map(|r| r.created_at).unwrap_or(0);
    out.push_str(&format!("  <updated>{}</updated>\n", rfc3339(newest)));
    for row in rows {
        let url = format!("{base}/thread/{}", row.thread_id);
        out.push_str("  <entry>\n");
        out.push_str(&format!("    <title>{}</title>\n", xml(&row.title)));
        out.push_str(&format!(
            "    <link rel=\"alternate\" href=\"{}\"/>\n",
            xml(&url)
        ));
        out.push_str(&format!(
            "    <id>tag:smrt,2026:notification:{}</id>\n",
            row.id
        ));
        out.push_str(&format!(
            "    <updated>{}</updated>\n",
            rfc3339(row.created_at)
        ));
        // uid 0 is the mirror's own break-glass hand rather than a person; the
        // panel says so too, and a feed that said "somebody" would be vaguer
        // than the truth
        let who = match (row.actor_login.as_deref(), row.actor_uid) {
            (Some(login), _) => login,
            (None, 0) => "the operator",
            _ => "somebody",
        };
        let what = match row.kind.as_str() {
            "comment" => format!("{who} said something"),
            "opened" => format!("{who} opened this on {}", row.pack_id),
            _ => format!("{who} marked it {}", row.status),
        };
        out.push_str(&format!("    <summary>{}</summary>\n", xml(&what)));
        out.push_str("  </entry>\n");
    }
    out.push_str("</feed>\n");
    out
}

/// The five characters that are not text in XML. Everything in the feed passes
/// through here, because everything in it was typed by somebody.
fn xml(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn rfc3339(unix: i64) -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// ── /v1/servers ────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/v1/servers",
    tag = "public",
    responses((status = 200, description = "Curated server listing", body = ServerListing))
)]
pub(crate) async fn list_servers(
    State(state): State<AppState>,
) -> Result<Json<ServerListing>, ApiError> {
    let servers = state.storage.list_servers().await?;
    Ok(Json(ServerListing {
        schema_version: SCHEMA_VERSION,
        generated_at: now_rfc3339(),
        servers,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/servers/{server_id}",
    tag = "public",
    params(("server_id" = String, Path, description = "Server identifier")),
    responses(
        (status = 200, body = ServerEntry),
        (status = 404, description = "No such server")
    )
)]
pub(crate) async fn get_server(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Json<ServerEntry>, ApiError> {
    Ok(Json(state.storage.load_server(&server_id).await?))
}

// ── /v1/featured ───────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/v1/featured",
    tag = "public",
    responses((status = 200, description = "Featured packs and servers", body = Featured))
)]
pub(crate) async fn get_featured(
    State(state): State<AppState>,
) -> Result<Json<Featured>, ApiError> {
    Ok(Json(state.storage.load_featured().await?))
}

// ── /v1/cache ──────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/v1/cache/{prefix}/{filename}",
    tag = "public",
    params(
        ("prefix" = String, Path, description = "First two hex chars of the sha1"),
        ("filename" = String, Path, description = "`<sha1>.jar`")
    ),
    responses(
        (status = 200, description = "The cached jar bytes"),
        (status = 404, description = "Not cached, or taken down")
    )
)]
pub(crate) async fn get_cache_jar(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path((prefix, filename)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let sha1 = filename
        .strip_suffix(".jar")
        .ok_or_else(|| ApiError::BadRequest("cache path must end in .jar".into()))?;
    // A taken-down jar must not be served even if its bytes are still on disk.
    if state.storage.is_sha1_removed(sha1).await? {
        return Err(ApiError::NotFound);
    }
    let path = state.storage.cache_jar_path(&prefix, sha1)?;
    if tokio::fs::metadata(&path).await.is_err() {
        return Err(ApiError::NotFound);
    }
    serve_file(&method, &headers, &path, "application/java-archive").await
}

// A cached mod's own embedded icon (mcmod.info logoFile / pack.png / fabric icon),
// so any browser of the registry -- guest, member, operator -- sees the real icon
// a self-hosted jar carries. Public alongside the jar itself: an icon extracted
// from a jar anyone can download is no more sensitive than the download. Immutable
// per sha1, so it caches hard; 404 when the jar has no icon (the caller falls back
// to a letter avatar).
#[utoipa::path(
    get,
    path = "/v1/cache/icon/{sha1}",
    tag = "public",
    params(("sha1" = String, Path, description = "Full 40-char sha1 of a cached jar")),
    responses(
        (status = 200, description = "The jar's embedded icon; immutable-cacheable"),
        (status = 404, description = "Jar not cached, or carries no icon")
    )
)]
pub(crate) async fn get_cache_icon(
    State(state): State<AppState>,
    Path(sha1): Path<String>,
) -> Result<Response, ApiError> {
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::BadRequest("sha1 must be 40 hex chars".into()));
    }
    // What a jar's icon is never changes -- the jar is addressed by its own
    // hash -- so it is extracted once and remembered, including the answer
    // "there is none", which is the common one and used to cost the same
    // megabytes to rediscover.
    let (img, content_type) = match state.storage.read_cached_icon(&sha1).await {
        IconRead::Image {
            bytes,
            content_type,
        } => (bytes, content_type),
        IconRead::Absent => return Err(ApiError::NotFound),
        IconRead::Unknown => {
            let path = state.storage.cache_jar_path(&sha1[..2], &sha1)?;
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|_| ApiError::NotFound)?;
            let icon = tokio::task::spawn_blocking(move || jar_icon(&bytes))
                .await
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("icon extract task: {e}")))??;
            state
                .storage
                .store_icon(&sha1, icon.as_ref().map(|(b, ct)| (b.as_slice(), *ct)))
                .await;
            icon.ok_or(ApiError::NotFound)?
        }
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            // bytes come from an untrusted jar; pin the type so the browser can't
            // sniff a "pack.png" that actually contains markup into something active
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        img,
    )
        .into_response())
}

/// A Modrinth project's icon, served from the mirror rather than from
/// Modrinth's CDN.
///
/// The panel used to receive the upstream url and put it straight into an
/// `<img>`, which meant every viewer's browser announced itself to a third
/// party once per icon on the page. The avatar proxy next door exists for
/// exactly that reason and this is the same act: fetch it once, keep it, serve
/// it from here.
///
/// Public, like every other picture the mirror serves. It costs an outbound
/// fetch only on a miss -- afterwards it is a file. A project the mirror cannot
/// resolve, or whose icon is a kind not served back, answers 404 and the caller
/// falls back to the jar's own icon or a letter.
#[utoipa::path(
    get,
    path = "/v1/modrinth/icon/{project_id}",
    tag = "public",
    params(("project_id" = String, Path, description = "Modrinth project id or slug")),
    responses(
        (status = 200, description = "The project's icon, held by the mirror"),
        (status = 404, description = "No icon for that project")
    )
)]
pub(crate) async fn get_project_icon(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Response, ApiError> {
    let (bytes, content_type) = match state.storage.read_project_icon(&project_id).await {
        Some(held) => held,
        None => {
            let url = state
                .modrinth
                .project_icon(&project_id)
                .await
                .map_err(|_| ApiError::NotFound)?
                .ok_or(ApiError::NotFound)?;
            let (bytes, content_type) = state
                .modrinth
                .fetch_image(&url)
                .await
                .map_err(|_| ApiError::NotFound)?;
            state
                .storage
                .store_project_icon(&project_id, &bytes, &content_type)
                .await;
            // read back rather than trust the upstream string: what is served is
            // one of the kinds the mirror is willing to serve, or nothing
            state
                .storage
                .read_project_icon(&project_id)
                .await
                .ok_or(ApiError::NotFound)?
        }
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            // a day, not forever: unlike a jar's icon this one can change upstream
            (header::CACHE_CONTROL, "public, max-age=86400"),
            // third-party bytes; pin the type so nothing can be sniffed into
            // something active
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/v1/cache/inventory",
    tag = "public",
    responses((status = 200, description = "Every cached artifact (sha1, size)", body = CacheInventory))
)]
pub(crate) async fn get_cache_inventory(
    State(state): State<AppState>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let after = page.cursor().and_then(|parts| parts.first().cloned());
    let entries = state
        .storage
        .cache_inventory_after(after.as_deref(), page.probe())
        .await?;
    // the envelope stays the envelope; only the rows in it are a page
    let (entries, next) = page.split(entries, |e| vec![e.sha1.clone()]);
    let mut resp = Json(CacheInventory {
        schema_version: SCHEMA_VERSION,
        generated_at: now_rfc3339(),
        entries,
    })
    .into_response();
    if let Some(value) = next_link(&uri, next.as_deref()) {
        resp.headers_mut().insert(header::LINK, value);
    }
    Ok(resp)
}

// ── /v1/users/:uid/avatar ──────────────────────────────────────────────────

/// Proxy a GitHub avatar through the mirror, keyed by the numeric uid we already
/// store. Serving it from our own origin means the panel never hotlinks
/// `avatars.githubusercontent.com` from the viewer's browser -- no viewer IP
/// handed to GitHub, no third-party origin on the page. A bad uid or an upstream
/// miss is a 404 the panel falls back from to a letter tile.
#[utoipa::path(
    get,
    path = "/v1/users/{uid}/avatar",
    tag = "public",
    params(("uid" = i64, Path, description = "GitHub numeric uid")),
    responses(
        (status = 200, description = "The avatar image, proxied through the mirror"),
        (status = 404, description = "Bad uid or upstream miss")
    )
)]
pub(crate) async fn get_user_avatar(
    State(state): State<AppState>,
    Path(uid): Path<i64>,
) -> Result<Response, ApiError> {
    if uid <= 0 {
        return Err(ApiError::NotFound);
    }
    // Held, like the project icons: without a copy the mirror asked GitHub once
    // per image per page load, which is a byline on every comment and a row in
    // every user list. The proxy is here so a viewer never reaches GitHub; it
    // should not mean the mirror reaches it instead, every time.
    let (bytes, content_type) = match state.storage.read_avatar(uid).await {
        Some(held) => held,
        None => {
            let url = format!("https://avatars.githubusercontent.com/u/{uid}?s=160&v=4");
            let (bytes, content_type) = state
                .modrinth
                .fetch_image(&url)
                .await
                .map_err(|_| ApiError::NotFound)?;
            state.storage.store_avatar(uid, &bytes, &content_type).await;
            // read back rather than trust the upstream string: what is served
            // is one of the kinds the mirror is willing to serve, or the bytes
            // as fetched when it is a kind it will not keep
            state
                .storage
                .read_avatar(uid)
                .await
                .unwrap_or((bytes, "image/png"))
        }
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
            // proxied third-party bytes: pin the type so the browser can't sniff
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Serve a file off disk, resumably.
///
/// A pack is hundreds of megabytes of jars fetched one at a time over whatever
/// connection the player has. Without ranges a download that dies at 90% starts
/// again from zero, which is the difference between a slow install and one that
/// never finishes on a bad line. The conditional headers come with it: a client
/// that already holds the file re-checks it without refetching it.
///
/// The range arithmetic is delegated rather than hand-rolled -- suffix ranges,
/// unsatisfiable ranges and `HEAD` are each a place to be subtly wrong, and a
/// subtly wrong range serves the wrong bytes rather than failing. The content
/// type stays ours: it is decided per route (`content_type_for`, and the fixed
/// archive type for a cache jar) and must not silently become whatever the
/// extension guesses.
///
/// `If-Range` is not among the headers passed on, because the file service does
/// not implement it: forwarding it would read as support for a conditional that
/// is silently ignored, which is worse than not offering it. A caller resuming
/// something that can change under them pairs the range with
/// `If-Unmodified-Since`, which is answered.
async fn serve_file(
    method: &Method,
    headers: &HeaderMap,
    path: &std::path::Path,
    content_type: &str,
) -> Result<Response, ApiError> {
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    // What the file service needs to answer conditionally; the rest of the
    // request is this handler's business and was already acted on.
    const CONDITIONAL: [header::HeaderName; 3] = [
        header::RANGE,
        header::IF_MODIFIED_SINCE,
        header::IF_UNMODIFIED_SINCE,
    ];
    let mut probe = axum::extract::Request::builder()
        .method(method.clone())
        .uri("/");
    for name in CONDITIONAL {
        if let Some(value) = headers.get(&name) {
            probe = probe.header(name, value);
        }
    }
    let probe = probe
        .body(Body::empty())
        .map_err(|e| ApiError::Internal(e.into()))?;

    let served = ServeFile::new(path)
        .oneshot(probe)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    if served.status() == StatusCode::NOT_FOUND {
        return Err(ApiError::NotFound);
    }
    let mut resp = served.map(Body::new);
    if let Ok(value) = header::HeaderValue::from_str(content_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    Ok(resp)
}

fn content_type_for(rel_path: &str) -> &'static str {
    let lower = rel_path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "json" => "application/json",
        "toml" => "application/toml",
        "txt" | "cfg" | "properties" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn now_rfc3339() -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ask for `path` the way a client would, optionally naming a byte range.
    async fn read(path: &std::path::Path, range: Option<&str>) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(r) = range {
            headers.insert(header::RANGE, r.parse().unwrap());
        }
        serve_file(&Method::GET, &headers, path, "application/java-archive")
            .await
            .expect("the file is there")
    }

    async fn body_of(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    fn a_file(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.jar");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn a_whole_file_comes_back_whole_and_offers_ranges() {
        let (_dir, path) = a_file(b"0123456789");
        let resp = read(&path, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/java-archive",
            "the type is the route's to decide, not the extension's to guess"
        );
        assert_eq!(body_of(resp).await, b"0123456789");
    }

    // The point of the whole thing: a download that died at 90% asks for the
    // rest rather than starting over.
    #[tokio::test]
    async fn a_download_resumes_where_it_stopped() {
        let (_dir, path) = a_file(b"0123456789");
        let resp = read(&path, Some("bytes=7-")).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 7-9/10"
        );
        assert_eq!(body_of(resp).await, b"789");
    }

    #[tokio::test]
    async fn a_range_is_the_bytes_it_names() {
        let (_dir, path) = a_file(b"0123456789");
        assert_eq!(body_of(read(&path, Some("bytes=2-5")).await).await, b"2345");
        // counted from the end
        assert_eq!(body_of(read(&path, Some("bytes=-3")).await).await, b"789");
    }

    // Conditional resume, for a file that can change under a caller: the range
    // is served only while the file has not moved. `If-Range` is deliberately
    // not honoured -- see serve_file -- so this is the header that answers.
    #[tokio::test]
    async fn a_range_can_be_made_conditional_on_the_file_not_moving() {
        let (_dir, path) = a_file(b"0123456789");
        let modified = read(&path, None)
            .await
            .headers()
            .get(header::LAST_MODIFIED)
            .expect("a served file dates itself")
            .clone();

        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=7-".parse().unwrap());
        headers.insert(header::IF_UNMODIFIED_SINCE, modified);
        let resp = serve_file(&Method::GET, &headers, &path, "application/java-archive")
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        // the same request against a file that has since moved on
        headers.insert(
            header::IF_UNMODIFIED_SINCE,
            "Fri, 09 Aug 1996 14:21:40 GMT".parse().unwrap(),
        );
        let resp = serve_file(&Method::GET, &headers, &path, "application/java-archive")
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::PRECONDITION_FAILED,
            "a moved file must not be spliced into a half-finished download"
        );
    }

    #[tokio::test]
    async fn a_range_past_the_end_is_refused_rather_than_answered() {
        let (_dir, path) = a_file(b"0123456789");
        let resp = read(&path, Some("bytes=50-60")).await;
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }
}
