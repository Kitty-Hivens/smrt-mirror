use super::ApiError;
use crate::accounts::{Identity, PackLevel, Role, UploadRow, UserRow};
use crate::authoring::{
    ResolveReport, ValidateReport, modrinth, pack_graph, reconstruct_config, resolve_pack, validate,
};
use crate::domain::*;
use crate::registry::model::GraphData;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, header};
use axum::middleware::from_fn_with_state;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};

use super::page::PageQuery;
use sha1::{Digest, Sha1};
use std::collections::HashMap;

use super::MAX_UPLOAD_BODY;

pub fn router(state: AppState) -> Router {
    operator_router(state.clone()).merge(authoring_router(state))
}

/// Operator-only surface: the official catalog, servers, cache management, and
/// user administration. Requires the admin role (and up).
fn operator_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/servers", post(save_server))
        .route("/v1/servers/{server_id}", delete(delete_server))
        .route("/v1/servers/{server_id}/advertised", get(advertised_mods))
        .route(
            "/v1/cache/{prefix}/{filename}",
            put(put_cache_jar).delete(delete_cache_jar),
        )
        .route("/v1/authoring/packs", get(list_authoring_packs))
        .route("/v1/authoring/summaries", get(list_all_pack_summaries))
        .route("/v1/featured", post(save_featured))
        .route("/v1/cache/removed", get(list_removed))
        .route(
            "/v1/cache/removed/{sha1}",
            post(takedown_jar).delete(restore_jar),
        )
        .route("/v1/cache/usage", get(list_cache_usage))
        .route("/v1/cache/github", post(ingest_github))
        .route("/v1/users", get(list_users))
        .route("/v1/users/{uid}/role", post(set_user_role))
        .route(
            "/v1/users/{uid}/suspension",
            post(suspend_account).delete(lift_suspension),
        )
        .route("/v1/uploads", get(list_uploads))
        .route("/v1/uploads/{id}/approve", post(approve_upload))
        .route("/v1/uploads/{id}/reject", post(reject_upload))
        .route("/v1/audit", get(get_audit_log))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY))
        .layer(from_fn_with_state(state.clone(), super::auth::require_auth))
        .with_state(state)
}

/// Pack-authoring surface: any signed-in member may reach it, but every
/// pack-scoped handler gates on `authorize` at the level that act needs (ADR
/// 0006) -- a member reaches their own community packs and whatever else they
/// were granted, officials stay admin-owned. The Modrinth proxy needs just a
/// session (no pack, no access).
fn authoring_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/v1/authoring/packs/{pack_id}/static/{*rel_path}",
            put(put_pack_static).delete(delete_pack_static),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/static",
            get(list_pack_static),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/config",
            get(get_pack_config).put(put_pack_config),
        )
        .route("/v1/meta/minecraft", get(get_minecraft_versions))
        .route("/v1/meta/loaders/{loader}", get(get_loader_versions))
        .route(
            "/v1/authoring/packs/{pack_id}/spoof",
            get(pack_spoof).post(generate_pack_spoof),
        )
        .route("/v1/authoring/packs/{pack_id}/events", get(pack_events))
        .route(
            "/v1/authoring/packs/{pack_id}/doc",
            get(get_pack_doc).post(post_pack_doc),
        )
        .route("/v1/authoring/packs/{pack_id}", delete(delete_pack))
        // Reading a discussion is not an authoring act -- it is as public as the
        // pack it is about, and lives on the public router. What is here is the
        // writing.
        .route("/v1/authoring/packs/{pack_id}/issues", post(open_issue))
        .route(
            "/v1/authoring/packs/{pack_id}/proposals",
            post(open_proposal),
        )
        .route("/v1/authoring/threads/{id}/comments", post(post_comment))
        .route("/v1/authoring/comments/{id}/hidden", put(moderate_comment))
        .route("/v1/authoring/threads/{id}/merge", post(merge_proposal))
        .route("/v1/authoring/threads/{id}/close", post(close_thread))
        .route("/v1/authoring/threads/{id}/reopen", post(reopen_thread))
        .route(
            "/v1/authoring/packs/{pack_id}/access",
            get(list_access).post(grant_access),
        )
        .route("/v1/authoring/packs/{pack_id}/access/mine", get(my_level))
        .route(
            "/v1/authoring/packs/{pack_id}/access/{uid}",
            delete(revoke_access),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/blocks",
            get(list_blocks).post(block_from_pack),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/blocks/{uid}",
            delete(unblock_from_pack),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/visibility",
            put(set_pack_visibility),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/config/revert",
            post(revert_pack_config),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits",
            get(list_commits).post(create_commit),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits/status",
            get(commit_status),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits/{commit_id}",
            get(get_commit),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits/{commit_id}/config",
            get(get_commit_config),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits/{commit_id}/diff",
            get(commit_diff),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/commits/{commit_id}/restore",
            post(restore_commit),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/duplicate",
            post(duplicate_pack),
        )
        // An instance archive in one body -- the only route on this router that
        // carries one, so the gigabyte ceiling stops here instead of applying to
        // every authoring write behind the same session gate.
        .route(
            "/v1/authoring/packs/{pack_id}/validate",
            post(validate_pack).layer(DefaultBodyLimit::max(super::MAX_ARCHIVE_BODY)),
        )
        .route(
            "/v1/authoring/packs/{pack_id}/dependency-preview",
            post(preview_dependencies),
        )
        .route("/v1/authoring/packs/{pack_id}/resolve", get(pack_resolve))
        .route("/v1/authoring/packs/{pack_id}/graph", get(pack_graph_view))
        .route("/v1/search/mods", get(search_mods_combined))
        .route("/v1/modrinth/search", get(modrinth_search))
        .route("/v1/modrinth/versions", get(modrinth_versions))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY))
        .layer(from_fn_with_state(
            state.clone(),
            super::auth::require_session,
        ))
        .with_state(state)
}

// ── audit ────────────────────────────────────────────────────────────────────

use super::audit;
use crate::authoring::spoof::SPOOF_DEST;

/// The audit trail, newest first -- the operator's "who did what" view.
///
/// Unpaged it answers the most recent 200, which is what it always did and all
/// the view shows. With `limit` it pages, and the trail becomes readable past
/// its head: the entry anyone goes looking for is usually not among the last
/// two hundred.
async fn get_audit_log(
    State(state): State<AppState>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let before = page
        .cursor()
        .and_then(|parts| parts.first()?.parse::<i64>().ok());
    let take = page.probe().unwrap_or(200) as i64;
    let acc = state.accounts.clone();
    let rows = tokio::task::spawn_blocking(move || acc.list_audit(take, before))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("audit task: {e}")))??;
    Ok(page.answer(rows, &uri, |r| vec![r.id.to_string()]))
}

// ── handlers ───────────────────────────────────────────────────────────────

/// Every registered user and their role, for the operator's user-management
/// view. Break-glass is excluded (it is a synthetic row, not a person).
async fn list_users(State(state): State<AppState>) -> Result<Json<Vec<UserRow>>, ApiError> {
    let acc = state.accounts.clone();
    let users = tokio::task::spawn_blocking(move || acc.list_users())
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("users task: {e}")))??;
    Ok(Json(users))
}

#[derive(serde::Deserialize)]
struct RoleReq {
    role: String,
}

#[derive(serde::Deserialize)]
struct SuspendReq {
    /// What the person is told when they try to write. Written for them.
    #[serde(default)]
    reason: Option<String>,
}

/// Stop an account from putting anything on the mirror.
///
/// The operators' answer, distinct from a pack's own block: a pack's keepers
/// decide who writes in their discussion, and no block on one pack answers
/// somebody whose pack was itself the offence.
///
/// It bars writing everywhere and touches reading nowhere. An operator cannot
/// be suspended -- the rung is the mirror's, and one operator silencing another
/// is a fight the gate should not host; demote them first if that is really the
/// intent.
async fn suspend_account(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(uid): Path<i64>,
    Json(req): Json<SuspendReq>,
) -> Result<StatusCode, ApiError> {
    if uid == identity.uid {
        return Err(ApiError::BadRequest(
            "suspending yourself would say nothing".into(),
        ));
    }
    let reason = tidy_reason(req.reason.as_deref())?;
    let acc = state.accounts.clone();
    let theirs = tokio::task::spawn_blocking(move || acc.identity_of(uid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("user task: {e}")))??;
    if theirs.map(|i| i.role >= Role::Admin).unwrap_or(false) {
        return Err(ApiError::BadRequest(
            "that account runs the mirror; take the rung away first".into(),
        ));
    }
    let acc = state.accounts.clone();
    let (by, note) = (identity.uid, reason.clone());
    tokio::task::spawn_blocking(move || acc.suspend_account(uid, note.as_deref(), by))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("suspension task: {e}")))?
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    audit(
        &state,
        &identity,
        "account.suspend",
        Some(&uid.to_string()),
        reason.as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Let an account put things on the mirror again.
async fn lift_suspension(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(uid): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let lifted = tokio::task::spawn_blocking(move || acc.lift_suspension(uid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("suspension task: {e}")))??;
    if lifted {
        audit(
            &state,
            &identity,
            "account.unsuspend",
            Some(&uid.to_string()),
            None,
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Set a user's role (member/admin) by GitHub uid.
async fn set_user_role(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(uid): Path<i64>,
    Json(req): Json<RoleReq>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let role = req.role.clone();
    tokio::task::spawn_blocking(move || acc.set_role(uid, &role))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("role task: {e}")))?
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    audit(
        &state,
        &identity,
        "role.set",
        Some(&uid.to_string()),
        Some(&req.role),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Pending member uploads awaiting moderation, oldest first.
async fn list_uploads(State(state): State<AppState>) -> Result<Json<Vec<UploadRow>>, ApiError> {
    let acc = state.accounts.clone();
    let rows = tokio::task::spawn_blocking(move || acc.list_pending_uploads())
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("uploads task: {e}")))??;
    Ok(Json(rows))
}

/// Approve a staged upload: promote its jar into the shared cache and mark it
/// approved. The registry is poked so the new artifact is harvested.
async fn approve_upload(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let upload = tokio::task::spawn_blocking(move || acc.get_upload(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("upload task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    already_decided(&upload)?;
    state.storage.promote_upload(&upload.sha1).await?;
    let acc = state.accounts.clone();
    let decided_by = identity.uid;
    tokio::task::spawn_blocking(move || {
        acc.set_upload_status(id, "approved", None, Some(decided_by))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("status task: {e}")))??;
    state.harvest.poke();
    state.events.moderation("decided");
    audit(
        &state,
        &identity,
        "upload.approve",
        Some(&upload.sha1),
        Some(&upload.pack_id),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct RejectBody {
    note: Option<String>,
}

/// An upload only gets decided once. Deciding a decided one again is not a
/// second opinion: rejecting an approved upload would mark it rejected while
/// its jar stayed in the shared cache -- promoted, referenced by whatever pack
/// pulled it, and now recorded as refused. The two moderation buttons live on a
/// queue two operators can be looking at, so this is a race worth naming rather
/// than a hypothetical.
fn already_decided(upload: &UploadRow) -> Result<(), ApiError> {
    if upload.status == "pending" {
        return Ok(());
    }
    Err(ApiError::Conflict(format!(
        "that upload was already {} -- reload the queue",
        upload.status
    )))
}

/// Reject a staged upload: drop its staged jar and mark it rejected, with an
/// optional moderator note the uploader sees.
async fn reject_upload(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Json(body): Json<RejectBody>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let upload = tokio::task::spawn_blocking(move || acc.get_upload(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("upload task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    already_decided(&upload)?;
    state.storage.discard_upload(&upload.sha1).await?;
    let acc = state.accounts.clone();
    let note = body.note.clone();
    let decided_by = identity.uid;
    tokio::task::spawn_blocking(move || {
        acc.set_upload_status(id, "rejected", note.as_deref(), Some(decided_by))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("status task: {e}")))??;
    state.events.moderation("decided");
    audit(
        &state,
        &identity,
        "upload.reject",
        Some(&upload.sha1),
        body.note.as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_server(
    State(state): State<AppState>,
    Json(entry): Json<ServerEntry>,
) -> Result<(StatusCode, Json<ServerEntry>), ApiError> {
    state.storage.save_server(&entry).await?;
    state.events.catalog("servers");
    Ok((StatusCode::CREATED, Json(entry)))
}

/// What the server itself says it runs (#110).
///
/// The spoof a pack ships has to claim exactly the list the server expects, and
/// the server states that list: on 1.12.2 Forge the FML handshake's mod list
/// also rides in the status ping. Asking is a status query -- no account, no
/// login, nothing a player could not do from the multiplayer screen -- so it can
/// be repeated whenever the answer matters instead of being pasted once and
/// trusted forever.
///
/// A server with no address recorded is a 400 rather than a silent empty list:
/// "we never asked" and "it answered nothing" are different facts.
async fn advertised_mods(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Json<crate::authoring::ServerStatus>, ApiError> {
    let entry = state.storage.load_server(&server_id).await?;
    let Some(address) = entry
        .address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    else {
        return Err(ApiError::BadRequest(format!(
            "server {server_id:?} has no address recorded, so there is nothing to ask"
        )));
    };
    let (host, port) = split_address(address)
        .ok_or_else(|| ApiError::BadRequest(format!("{address:?} is not an address")))?;
    crate::authoring::server_status(&host, port)
        .await
        .map(Json)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("asking {address}: {e:#}")))
}

/// The claim a pack ships, the claim its server expects now, and what moved
/// between them (#110).
///
/// The freshness question the hand-written file could never answer: a server
/// bumps its mod list, the shipped claim keeps naming the old one, and the only
/// symptom is a refused handshake. Asking is cheap and repeatable, so it is a
/// report rather than something anyone has to remember to check.
async fn pack_spoof(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<SpoofReport>, ApiError> {
    use crate::authoring::spoof::{Spoof, drift};
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;

    // What the pack ships, if it ships one: the asset the launcher writes into
    // the instance. Absent is a state, not a failure -- most packs need no claim.
    let shipped: Option<Spoof> = match cfg.assets.iter().find(|a| a.dest == SPOOF_DEST) {
        Some(crate::domain::DeclaredAsset {
            source: crate::domain::SourceDecl::SmrtStatic { rel_path },
            ..
        }) => {
            let path = state.storage.pack_static_path(&pack_id, rel_path)?;
            tokio::fs::read(&path)
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        }
        _ => None,
    };

    // Which server this pack joins. Without one there is nothing to compare
    // against, and the shipped claim is reported alone rather than judged.
    let server_id = cfg.auth.as_ref().and_then(|a| a.server_id.clone());
    let (current, asked, unasked) = ask_packs_server(&state, &cfg).await;

    let moved = match (&shipped, &current) {
        (Some(a), Some(b)) => drift(a, b),
        _ => Vec::new(),
    };
    Ok(Json(SpoofReport {
        shipped,
        current,
        server_id,
        asked,
        unasked: unasked.map(str::to_string),
        drift: moved,
    }))
}

/// host, or host:port -- the default is Minecraft's, not the caller's problem.
fn split_address(address: &str) -> Option<(String, u16)> {
    let address = address.trim();
    if address.is_empty() {
        return None;
    }
    match address.rsplit_once(':') {
        Some((h, p)) => p.parse::<u16>().ok().map(|port| (h.to_string(), port)),
        None => Some((address.to_string(), 25565)),
    }
}

/// Where the pack's claim has to come from, and what that server says now.
/// Shared by the report and the write, so the two cannot disagree about which
/// server a pack answers to.
async fn ask_packs_server(
    state: &AppState,
    cfg: &crate::domain::PackConfig,
) -> (
    Option<crate::authoring::spoof::Spoof>,
    Option<String>,
    Option<&'static str>,
) {
    use crate::authoring::spoof::spoof_from_status;
    let Some(id) = cfg.auth.as_ref().and_then(|a| a.server_id.as_deref()) else {
        return (
            None,
            None,
            Some("this pack names no server, so there is nothing it has to match"),
        );
    };
    let Ok(entry) = state.storage.load_server(id).await else {
        return (
            None,
            None,
            Some("this pack names a server the mirror has no entry for"),
        );
    };
    let Some((host, port)) = entry.address.as_deref().and_then(split_address) else {
        return (
            None,
            None,
            Some("that server has no address recorded, so there is nothing to ask"),
        );
    };
    match crate::authoring::server_status(&host, port).await {
        Ok(status) => {
            let asked = Some(format!("{host}:{port}"));
            match spoof_from_status(&status) {
                Ok(spoof) => (Some(spoof), asked, None),
                Err(_) => (
                    None,
                    asked,
                    Some("that server answered without advertising a mod list"),
                ),
            }
        }
        Err(e) => {
            tracing::warn!(pack = %cfg.pack_id, error = %format!("{e:#}"), "asking the pack's server failed");
            (
                None,
                None,
                Some("that server could not be reached just now"),
            )
        }
    }
}

/// Where a generated claim is written under the pack's static tree. Its own
/// directory: this file is the mirror's output, not something a curator placed,
/// and a regeneration overwrites it without touching anything hand-uploaded.
const SPOOF_REL_PATH: &str = "_generated/hidemymods-spoof.json";

/// Write the pack's handshake claim from what its server says now (#110).
///
/// Replaces the shipped file rather than merging with it: the claim is the
/// server's list, whole, and a claim half-derived from two moments in time is
/// the stale file this exists to abolish.
async fn generate_pack_spoof(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<SpoofReport>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let _guard = state.storage.lock_pack_config(&pack_id).await;
    let mut cfg = state.storage.load_pack_config(&pack_id).await?;
    let (current, asked, unasked) = ask_packs_server(&state, &cfg).await;
    let Some(spoof) = current else {
        return Err(ApiError::BadRequest(
            unasked
                .unwrap_or("there is nothing to write a claim from")
                .to_string(),
        ));
    };

    let bytes = serde_json::to_vec_pretty(&spoof)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("encoding the claim: {e}")))?;
    state
        .storage
        .save_pack_static(&pack_id, SPOOF_REL_PATH, &bytes)
        .await?;

    // Declared once and kept: a second row writing the same destination is the
    // duplicate the config refuses, and re-pointing the existing one is what a
    // regeneration means anyway.
    let declaration = crate::domain::DeclaredAsset {
        dest: SPOOF_DEST.to_string(),
        required: true,
        source: crate::domain::SourceDecl::SmrtStatic {
            rel_path: SPOOF_REL_PATH.to_string(),
        },
        display: None,
    };
    match cfg.assets.iter_mut().find(|a| a.dest == SPOOF_DEST) {
        Some(existing) => *existing = declaration,
        None => cfg.assets.push(declaration),
    }

    // An operator action that rewrites the config has to settle the live
    // document, exactly as a revert does: it would otherwise put back the
    // version its editors still hold and undo this.
    state.docs.forget(&pack_id);
    let server_id = cfg.auth.as_ref().and_then(|a| a.server_id.clone());
    let (_, _) = store_edited_config(&state, &pack_id, cfg, &identity.login, false).await?;
    audit(
        &state,
        &identity,
        "pack.spoof",
        Some(&pack_id),
        Some(&format!(
            "{} mods claimed from {}",
            spoof.mods.len(),
            asked.as_deref().unwrap_or("its server")
        )),
    )
    .await;
    Ok(Json(SpoofReport {
        shipped: Some(spoof.clone()),
        current: Some(spoof),
        server_id,
        asked,
        unasked: None,
        drift: Vec::new(),
    }))
}

/// What a pack claims, what its server wants, and the difference.
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub struct SpoofReport {
    /// The claim the pack ships. Absent when it ships none, which is the
    /// ordinary case -- most packs match their server without help.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub shipped: Option<crate::authoring::spoof::Spoof>,
    /// What the server advertises now. Absent when the pack names no server,
    /// the server has no address recorded, or it could not be reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current: Option<crate::authoring::spoof::Spoof>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_id: Option<String>,
    /// The address actually asked, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asked: Option<String>,
    /// Why the server was not asked, or why its answer yielded nothing. Absent
    /// when it was asked and answered: then an empty `drift` means they agree,
    /// which is the one case worth telling apart from every kind of silence.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unasked: Option<String>,
    /// Empty when they agree, or when there was nothing to compare.
    pub drift: Vec<String>,
}

async fn delete_server(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.storage.delete_server(&server_id).await?;
    state.events.catalog("servers");
    Ok(StatusCode::NO_CONTENT)
}

async fn put_cache_jar(
    State(state): State<AppState>,
    Path((prefix, filename)): Path<(String, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<PutCacheResponse>), ApiError> {
    let sha1 = filename
        .strip_suffix(".jar")
        .ok_or_else(|| ApiError::BadRequest("cache path must end in .jar".into()))?;
    if !sha1.starts_with(&prefix) {
        return Err(ApiError::BadRequest("prefix does not match sha1".into()));
    }
    state.storage.save_cache_jar(sha1, &body).await?;
    state.harvest.poke(); // new artifact -> refresh the registry
    Ok((
        StatusCode::CREATED,
        Json(PutCacheResponse {
            schema_version: SCHEMA_VERSION,
            sha1: sha1.to_string(),
            size_bytes: body.len() as u64,
        }),
    ))
}

async fn delete_cache_jar(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((prefix, filename)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let sha1 = filename
        .strip_suffix(".jar")
        .ok_or_else(|| ApiError::BadRequest("cache path must end in .jar".into()))?;
    if !sha1.starts_with(&prefix) {
        return Err(ApiError::BadRequest("prefix does not match sha1".into()));
    }
    state.storage.delete_cache_jar(sha1).await?;
    state.harvest.poke(); // artifact gone -> refresh the registry
    audit(&state, &identity, "cache.delete", Some(sha1), None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Block a jar (copyright / policy): drop any cached copy and tombstone the sha1
/// so it can neither be served nor re-ingested. Deliberate and reversible via
/// `restore`; distinct from delete, which only frees bytes (#14).
async fn takedown_jar(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(sha1): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.storage.takedown(&sha1).await?;
    state.harvest.poke();
    audit(&state, &identity, "cache.takedown", Some(&sha1), None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Lift a takedown: remove the sha1 from the removed list. The bytes are not
/// restored -- re-add the jar to recache it.
async fn restore_jar(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(sha1): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.storage.restore(&sha1).await?;
    state.harvest.poke();
    audit(&state, &identity, "cache.restore", Some(&sha1), None).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn put_pack_static(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, rel_path)): Path<(String, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<PutStaticResponse>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    state
        .storage
        .save_pack_static(&pack_id, &rel_path, &body)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PutStaticResponse {
            schema_version: SCHEMA_VERSION,
            pack_id,
            rel_path,
            size_bytes: body.len() as u64,
        }),
    ))
}

async fn delete_pack_static(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, rel_path)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    state
        .storage
        .delete_pack_static(&pack_id, &rel_path)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_pack_static(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<StaticListing>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let files = state.storage.list_pack_static(&pack_id).await?;
    Ok(Json(StaticListing {
        schema_version: SCHEMA_VERSION,
        pack_id,
        files,
    }))
}

async fn save_featured(
    State(state): State<AppState>,
    Json(featured): Json<Featured>,
) -> Result<(StatusCode, Json<Featured>), ApiError> {
    state.storage.save_featured(&featured).await?;
    state.events.catalog("featured");
    Ok((StatusCode::CREATED, Json(featured)))
}

#[derive(serde::Deserialize)]
struct ModSearchQuery {
    q: String,
    mc: Option<String>,
    loader: Option<String>,
    /// The pack being filled. Its declared mods decide whether a foreign-loader
    /// hit is carried by a bridge already in the pack or would need one added --
    /// two different answers that must not be flattened into "incompatible".
    pack: Option<String>,
    limit: Option<usize>,
}

/// One search over both places a mod can come from (#101).
///
/// Provenance is the mirror's problem, not a question to ask before the question:
/// a hit says whether the mirror holds the bytes, and the caller does not have to
/// pick a door first. Neither loader nor Minecraft version filters -- a mod
/// riding a bridge loads, and hiding it would be a lie -- they rank, and each hit
/// carries the verdict it was ranked by.
async fn search_mods_combined(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<ModSearchQuery>,
) -> Result<Json<Vec<crate::authoring::ModHit>>, ApiError> {
    // The pack is optional, but reading one is authoring: gate it the same way
    // the rest of the authoring surface is.
    let present: std::collections::HashSet<String> = match &q.pack {
        Some(pack_id) => {
            super::auth::authorize(&state, &identity, pack_id, PackLevel::View).await?;
            state
                .storage
                .load_pack_config(pack_id)
                .await
                .map(|cfg| {
                    cfg.mods
                        .iter()
                        .filter_map(|m| match &m.source {
                            SourceDecl::Modrinth { project_id, .. } => Some(project_id.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        None => Default::default(),
    };
    let cached: std::collections::HashSet<String> = state
        .storage
        .list_cache_inventory()
        .await
        .map(|inv| inv.into_iter().map(|e| e.sha1).collect())
        .unwrap_or_default();

    let ctx = crate::authoring::PackContext {
        mc: q.mc.as_deref(),
        loader: q.loader.as_deref(),
        present_projects: &present,
    };
    let hits = crate::authoring::search_mods(
        &q.q,
        &ctx,
        &cached,
        q.limit.unwrap_or(30).clamp(1, 100),
        &state.registry,
        &state.modrinth,
    )
    .await
    .map_err(ApiError::Internal)?;
    Ok(Json(hits))
}

// ── Modrinth proxy (search-to-add) ──────────────────────────────────────────

#[derive(serde::Deserialize)]
struct SearchQuery {
    q: String,
    mc: Option<String>,
    // Modrinth project kind: mod (default) / resourcepack / shader, so the
    // panel can browse packs for assets, not just mods.
    #[serde(rename = "type")]
    kind: Option<String>,
}

async fn modrinth_search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<modrinth::SearchHit>>, ApiError> {
    // clamp to the Modrinth project kinds we support; unknown -> mod
    let kind = match q.kind.as_deref() {
        Some("resourcepack") => "resourcepack",
        Some("shader") => "shader",
        _ => "mod",
    };
    let hits = state
        .modrinth
        .search(&q.q, q.mc.as_deref(), kind)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(hits))
}

#[derive(serde::Deserialize)]
struct VersionsQuery {
    id: String,
    mc: Option<String>,
}

async fn modrinth_versions(
    State(state): State<AppState>,
    Query(q): Query<VersionsQuery>,
) -> Result<Json<Vec<modrinth::Version>>, ApiError> {
    let vs = state
        .modrinth
        .project_versions(&q.id, q.mc.as_deref())
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(vs))
}

// ── validate against an instance archive ─────────────────────────────────────

// Cross-reference the saved config against an uploaded instance archive by mod
// filename. spawn_blocking: unzipping a large archive must not stall the runtime.
async fn validate_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    body: Bytes,
) -> Result<Json<ValidateReport>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let report = tokio::task::spawn_blocking(move || validate(&cfg, &body))
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .map_err(ApiError::Internal)?;
    Ok(Json(report))
}

/// What saving this config would pull in, answered before the save.
///
/// Adding a mod landed blind: the dependencies appeared later, on save, and the
/// curator learned what came with it after the fact (#53). The plan was already
/// computed on every save; this asks for it in advance. Read-only -- it runs the
/// real fill on a copy, so the answer cannot drift from what the save does, and
/// writes nothing.
///
/// The body is the config as the editor currently holds it, not the stored one:
/// the question is about the mod just added, which by definition is not saved.
async fn preview_dependencies(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(cfg): Json<PackConfig>,
) -> Result<Json<Vec<crate::authoring::depfill::PulledPreview>>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cached: std::collections::HashSet<String> = state
        .storage
        .list_cache_inventory()
        .await
        .map(|inv| inv.into_iter().map(|e| e.sha1).collect())
        .unwrap_or_default();
    let pulled =
        crate::authoring::depfill::preview_fill(&cfg, &state.registry, &state.modrinth, &cached)
            .await
            .map_err(ApiError::Internal)?;
    Ok(Json(pulled))
}

// ── resolve against the dependency graph ─────────────────────────────────────

// Read the saved config and check it against the registry dependency graph:
// unmet hard deps, active conflicts, capability overlaps, version windows, and
// which declared mods are depended-on. Read-only -- it never edits the config,
// so required/optional stays the pack's decision. spawn_blocking: the registry
// is a synchronous SQLite handle.
async fn pack_resolve(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<ResolveReport>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let registry = state.registry.clone();
    let for_registry = cfg.clone();
    let mut report =
        tokio::task::spawn_blocking(move || registry.with_conn(|c| resolve_pack(c, &for_registry)))
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("resolve task: {e}")))?
            .map_err(ApiError::Internal)?;
    // The one finding the registry cannot answer on its own: what each jar
    // demands of the loader build, which lives inside the jar (#164). Run here
    // as well as at build time so the editor shows it while the pin is being
    // chosen, rather than at the end of a publish.
    crate::authoring::loader_windows(&cfg, state.storage.root(), &state.registry, &state.modrinth)
        .await
        .apply(&mut report);
    Ok(Json(report))
}

/// The pack's own relation graph: its mods, wired by what the exact artifacts it
/// ships declare. The registry-wide graph answers "what does the mirror hold"; this
/// answers "does this pack hold together", which is the question being asked while
/// a pack is authored. Same shape as the registry graph, so the panel renders it
/// with the same view -- an edge that dangles here is a requirement the pack does
/// not carry.
async fn pack_graph_view(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<GraphData>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let registry = state.registry.clone();
    let graph = tokio::task::spawn_blocking(move || registry.with_conn(|c| pack_graph(c, &cfg)))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("pack graph task: {e}")))?
        .map_err(ApiError::Internal)?;
    Ok(Json(graph))
}

// ── removed-list (takedown) ──────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct RemovedListing {
    schema_version: u32,
    removed: Vec<String>,
}

async fn list_removed(State(state): State<AppState>) -> Result<Json<RemovedListing>, ApiError> {
    let removed = state.storage.list_removed().await?;
    Ok(Json(RemovedListing {
        schema_version: SCHEMA_VERSION,
        removed,
    }))
}

// Enrich the cache inventory with where each jar is used, by reverse-indexing
// every authoring config's smrt_cache sources. Admin-only: it exposes which
// pack pulls which jar (and under what filename), which the public inventory
// must not. A jar with no uses is an orphan -- safe to take down.
async fn list_cache_usage(
    State(state): State<AppState>,
) -> Result<Json<CacheUsageListing>, ApiError> {
    let inventory = state.storage.list_cache_inventory().await?;
    let pack_ids = state.storage.list_authoring_packs().await?;

    let mut uses: HashMap<String, Vec<CacheUse>> = HashMap::new();
    for pid in pack_ids {
        let cfg = match state.storage.load_pack_config(&pid).await {
            Ok(c) => c,
            // a pack whose config is missing OR malformed just contributes no
            // uses; one unreadable config must not sink the whole listing
            Err(e) => {
                tracing::warn!(pack = %pid, error = %e, "skipping pack in cache usage");
                continue;
            }
        };
        for m in &cfg.mods {
            if let SourceDecl::SmrtCache { sha1 } = &m.source {
                uses.entry(sha1.clone()).or_default().push(CacheUse {
                    pack_id: pid.clone(),
                    filename: m.filename.clone(),
                });
            }
        }
        for a in &cfg.assets {
            if let SourceDecl::SmrtCache { sha1 } = &a.source {
                uses.entry(sha1.clone()).or_default().push(CacheUse {
                    pack_id: pid.clone(),
                    filename: a.dest.clone(),
                });
            }
        }
    }

    let entries = inventory
        .into_iter()
        .map(|e| {
            let uses = uses.remove(&e.sha1).unwrap_or_default();
            CacheUsageEntry {
                sha1: e.sha1,
                size_bytes: e.size_bytes,
                uses,
            }
        })
        .collect();
    Ok(Json(CacheUsageListing {
        schema_version: SCHEMA_VERSION,
        entries,
    }))
}

#[derive(serde::Deserialize)]
struct GithubIngest {
    repo: String,
    tag: String,
    asset: String,
}

fn safe_seg(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// A single URL path segment for a release tag / asset name. Real GitHub release
/// filenames carry spaces, parens, commas etc, so this only rejects what would
/// change the URL's shape -- path separators, `.`/`..` traversal, control chars,
/// emptiness. Everything else is allowed and percent-encoded into the URL.
fn safe_path_seg(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.chars().any(|c| c.is_control())
}

/// Percent-encode one URL path segment: keep the RFC 3986 unreserved set, encode
/// space and everything URL-structural so a filename with spaces/`&`/`+`/`#`
/// produces a valid, unambiguous path.
fn enc_seg(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}')
        .add(b'/')
        .add(b'&')
        .add(b'=')
        .add(b'+');
    utf8_percent_encode(s, SET).to_string()
}

/// Build the github.com release-download URL from a repo / tag / asset, or `None`
/// if the inputs aren't safe. `repo` accepts a pasted URL or `owner/name` and is
/// kept strict (it's the SSRF-sensitive path prefix); tag/asset may be richer
/// filenames and are percent-encoded.
fn github_asset_url(repo_in: &str, tag_in: &str, asset_in: &str) -> Option<String> {
    let repo = repo_in
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let tag = tag_in.trim();
    let asset = asset_in.trim();
    let repo_ok = repo.matches('/').count() == 1 && repo.split('/').all(safe_seg);
    if !repo_ok || !safe_path_seg(tag) || !safe_path_seg(asset) {
        return None;
    }
    Some(format!(
        "https://github.com/{repo}/releases/download/{}/{}",
        enc_seg(tag),
        enc_seg(asset)
    ))
}

// Fetch a GitHub release asset server-side and cache it by content hash, so a
// pack can pull a GitHub-only mod (open-smrt-network, hidemymods) as a normal
// smrt_cache source -- no new wire source type. The host is fixed to github.com,
// repo is kept strict (owner/name), and tag/asset are single, percent-encoded
// path segments (no separators, no traversal) -- not an open SSRF sink. See
// `github_asset_url`.
async fn ingest_github(
    State(state): State<AppState>,
    Json(req): Json<GithubIngest>,
) -> Result<(StatusCode, Json<PutCacheResponse>), ApiError> {
    let url = github_asset_url(&req.repo, &req.tag, &req.asset).ok_or_else(|| {
        ApiError::BadRequest(
            "repo must be owner/name (a github.com URL is ok); tag and asset must each be a single path segment".into(),
        )
    })?;
    let bytes = state
        .modrinth
        .fetch_bytes(&url)
        .await
        .map_err(ApiError::Internal)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let sha1 = hex::encode(hasher.finalize());
    state.storage.save_cache_jar(&sha1, &bytes).await?;
    state.harvest.poke(); // new artifact -> refresh the registry
    Ok((
        StatusCode::CREATED,
        Json(PutCacheResponse {
            schema_version: SCHEMA_VERSION,
            sha1,
            size_bytes: bytes.len() as u64,
        }),
    ))
}

// ── authoring inputs ───────────────────────────────────────────────────────

async fn list_authoring_packs(
    State(state): State<AppState>,
) -> Result<Json<AuthoringPacksListing>, ApiError> {
    let packs = state.storage.list_authoring_packs().await?;
    Ok(Json(AuthoringPacksListing {
        schema_version: SCHEMA_VERSION,
        packs,
    }))
}

/// What is happening to a pack while it is open, as a stream (#52 follow-on).
///
/// The revision check refuses a save that would overwrite someone else's, which
/// stops the loss and says nothing until the collision: you learn someone else
/// is here by colliding with them. This says it earlier -- who else has the pack
/// open, and that it moved, the moment it moves.
///
/// A save publishes the revision it produced, which is the same string
/// `If-Match` compares, so an editor can tell its own save from someone else's
/// without asking the server anything.
async fn pack_events(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    use axum::response::sse::{Event, KeepAlive, Sse};
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    // The subscription is held by the stream itself: when the client goes away
    // the stream drops, the guard drops with it, and the leave is announced --
    // no separate goodbye to miss.
    let mut presence = state.packs.join(&pack_id, &identity.login);
    let events = async_stream::stream! {
        loop {
            match presence.events.recv().await {
                Ok(event) => {
                    let data = serde_json::to_string(&event).unwrap_or_default();
                    yield Ok::<Event, std::convert::Infallible>(Event::default().event("pack").data(data));
                }
                // Lagged: this client fell behind the backlog. Nothing is lost
                // that matters -- the editor re-reads on the next event, and the
                // revision tells it whether anything moved -- so keep going.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Every build of a loader a pack can be pinned to (#126).
///
/// A loader with no published list -- a fork the registry knows through its
/// loader chain -- answers 404 rather than an empty list: "nothing to offer" and
/// "no builds exist" are different, and the field falls back to free text on
/// the first, which is what it always was.
async fn get_loader_versions(
    State(state): State<AppState>,
    Path(loader): Path<String>,
) -> Result<Json<crate::authoring::LoaderVersions>, ApiError> {
    if !crate::authoring::loaders::is_known(&loader) {
        return Err(ApiError::NotFound);
    }
    crate::authoring::loader_versions(&state.storage, &state.modrinth, &loader)
        .await
        .map(Json)
        .map_err(ApiError::Internal)
}

/// Every Minecraft version a pack can be built against (#126).
///
/// Served from the mirror's own copy so opening the editor does not depend on
/// somebody else's service, and so an outage narrows the offer to what was last
/// known rather than emptying it -- an empty picker sends the operator back to
/// typing, which is what this replaced.
async fn get_minecraft_versions(
    State(state): State<AppState>,
) -> Result<Json<crate::authoring::MinecraftVersions>, ApiError> {
    crate::authoring::minecraft_versions(&state.storage, &state.modrinth)
        .await
        .map(Json)
        .map_err(ApiError::Internal)
}

/// How long the mirror waits for the typing to stop before writing the merged
/// config to disk. Long enough that a sentence is one write rather than forty,
/// short enough that a build started right after an edit sees it.
const MATERIALISE_AFTER: std::time::Duration = std::time::Duration::from_millis(1200);

/// The document as it stands, for an editor joining the pack.
///
/// Binary rather than JSON: this is a CRDT update, not a config. The editor
/// applies it to an empty document -- seeding its own from the config and then
/// applying this would author a second value for every key, and one whole `mods`
/// array would silently replace the other.
async fn get_pack_doc(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let doc = state
        .docs
        .get_or_seed(&pack_id, &cfg)
        .map_err(ApiError::Internal)?;
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        doc.state(),
    ))
}

/// One editor's changes. Merged into the mirror's copy, passed on to everyone
/// else in the pack, and written to disk once the typing stops.
async fn post_pack_doc(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    // The config is read only when there is no document yet: seeding happens
    // once per pack, and a file read per keystroke would be the cost of writing
    // this the obvious way.
    let doc = match state.docs.get(&pack_id) {
        Some(doc) => doc,
        None => {
            let cfg = state.storage.load_pack_config(&pack_id).await?;
            state
                .docs
                .get_or_seed(&pack_id, &cfg)
                .map_err(ApiError::Internal)?
        }
    };
    doc.apply(&body)
        .map_err(|e| ApiError::BadRequest(format!("{e:#}")))?;

    // Everyone else in the pack gets the same bytes. Base64 because the room is
    // server-sent events, which is a text protocol; the alternative is a second
    // transport for one field.
    use base64::Engine;
    state.packs.publish(
        &pack_id,
        crate::authoring::PackEvent::Doc {
            by: identity.login.clone(),
            update: base64::engine::general_purpose::STANDARD.encode(&body),
        },
    );

    // Writing on every keystroke would be a file write per character and a
    // revision bump per character with it. Each update takes a ticket; the last
    // one still holding it when the wait is over does the write.
    let ticket = state.docs.touch(&pack_id);
    let (state, pack_id, by) = (state.clone(), pack_id.clone(), identity.login.clone());
    tokio::spawn(async move {
        tokio::time::sleep(MATERIALISE_AFTER).await;
        if !state.docs.is_current(&pack_id, ticket) {
            return;
        }
        materialise(&state, &pack_id, &by).await;
    });
    Ok(StatusCode::ACCEPTED)
}

/// Write the merged document to `config.json`, through the same door a
/// whole-config save uses.
///
/// Everything that reads a pack keeps reading that file, which is the whole
/// reason this arrangement is affordable: the document is a merge point, not a
/// second source of truth.
async fn materialise(state: &AppState, pack_id: &str, by: &str) {
    let _guard = state.storage.lock_pack_config(pack_id).await;
    let Some(doc) = state.docs.get(pack_id) else {
        return;
    };
    let Ok(existing) = state.storage.load_pack_config(pack_id).await else {
        return;
    };
    // Two people can leave the document briefly nonsensical -- a half-retyped
    // version string is not a config. Keep the last good file and try again on
    // the next edit rather than writing half of one.
    let cfg = match doc.to_config(&existing) {
        Ok(mut cfg) => {
            // Rows the fill appended after this document was seeded are not in
            // it, and writing the document as-is would delete them. They are
            // server-managed, so they come from the file the same way a
            // whole-config save carries them over; the document picks them up
            // when it is next seeded.
            crate::authoring::depfill::merge_pulled(&existing, &mut cfg);
            cfg
        }
        Err(e) => {
            tracing::warn!(pack_id, error = %format!("{e:#}"), "document does not materialise; keeping the stored config");
            return;
        }
    };
    if serde_json::to_value(&cfg).ok() == serde_json::to_value(&existing).ok() {
        return; // nothing moved; a write would bump the revision for nobody
    }
    let fill = fill_needed(Some(&existing), &cfg);
    if let Err(e) = store_edited_config(state, pack_id, cfg, by, fill).await {
        tracing::warn!(pack_id, error = ?e, "materialising the document failed");
    }
}

// ── config revisions (optimistic concurrency) ───────────────────────────────
//
// Config edits used to be a plain read-modify-write: two accounts editing one
// pack each saved against their own snapshot, and whoever saved second
// overwrote the first with no error and no trace (#52). A config now carries an
// `ETag` of its authored content, and a save may state which revision it edited
// via `If-Match`. A save whose base no longer matches disk is refused instead of
// applied, so the loser re-reads and reapplies rather than losing the work
// silently. The header is optional: a script or CLI that sends none writes
// unconditionally, exactly as before.

/// What a conditional write requires of the stored config: any config at all
/// (`If-Match: *`), or one specific revision.
enum Precondition {
    Any,
    Rev(String),
}

/// The `If-Match` precondition on this request, if it carries one. Entity tags
/// are opaque strings here, so the quoting and any weak-validator prefix are
/// stripped and what remains is compared verbatim. Only the single-tag form is
/// understood -- a tag list is not a shape this API's own clients produce, and
/// treating one as a mismatch refuses the write rather than waving it through.
fn precondition(headers: &HeaderMap) -> Option<Precondition> {
    let raw = headers.get(header::IF_MATCH)?.to_str().ok()?.trim();
    if raw == "*" {
        return Some(Precondition::Any);
    }
    let tag = raw
        .strip_prefix("W/")
        .unwrap_or(raw)
        .trim_matches('"')
        .to_string();
    (!tag.is_empty()).then_some(Precondition::Rev(tag))
}

/// Whether a conditional write may proceed against the revision now on disk
/// (`None` = the pack has no stored config).
fn check_precondition(
    pre: Option<&Precondition>,
    current: Option<&str>,
    pack_id: &str,
) -> Result<(), ApiError> {
    match (pre, current) {
        (None, _) => Ok(()),
        (Some(Precondition::Any), Some(_)) => Ok(()),
        (Some(Precondition::Rev(want)), Some(now)) if want == now => Ok(()),
        (Some(Precondition::Rev(want)), Some(now)) => Err(ApiError::Conflict(format!(
            "pack {pack_id:?} was saved by someone else since this edit began \
             (edited {}, now {}); reload the config and reapply the change",
            short_rev(want),
            short_rev(now)
        ))),
        (Some(_), None) => Err(ApiError::Conflict(format!(
            "pack {pack_id:?} has no stored config to match against; it was deleted or never existed"
        ))),
    }
}

/// A revision in an operator-facing message: enough to tell two apart, short
/// enough to read.
fn short_rev(rev: &str) -> &str {
    rev.get(..8).unwrap_or(rev)
}

/// The config's revision, or an internal error -- a config that cannot be
/// serialized has no revision to compare, and answering with one that always
/// matches would put the silent overwrite back.
fn rev_of(cfg: &PackConfig) -> Result<String, ApiError> {
    cfg.edit_rev()
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("config rev: {e}")))
}

/// The `ETag` response header carrying a config revision.
fn etag(rev: &str) -> [(HeaderName, String); 1] {
    [(header::ETAG, format!("\"{rev}\""))]
}

async fn get_pack_config(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<([(HeaderName, String); 1], Json<PackConfig>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let rev = rev_of(&cfg)?;
    Ok((etag(&rev), Json(cfg)))
}

async fn put_pack_config(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    headers: HeaderMap,
    Json(mut cfg): Json<PackConfig>,
) -> Result<(StatusCode, [(HeaderName, String); 1], Json<PackConfig>), ApiError> {
    // The path id is authoritative; reject a body that disagrees so a
    // mis-targeted PUT can't write one pack's config under another's id.
    if cfg.pack_id != pack_id {
        return Err(ApiError::BadRequest(format!(
            "body pack_id {:?} does not match path {:?}",
            cfg.pack_id, pack_id
        )));
    }
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    // One row per mod and per asset dest: the same artifact twice, or two rows
    // writing one path, is an authoring mistake no build can act on. Checked on
    // the incoming body, before anything server-side touches it.
    if let Some(dup) = cfg.duplicate_declaration() {
        return Err(ApiError::BadRequest(dup));
    }
    let pre = precondition(&headers);
    // From here to the write, this pack's config is ours alone: the stored
    // config is read, its server-controlled fields and pulled dependencies are
    // carried over, and the result is written. Two of those interleaving is the
    // same lost update `If-Match` refuses, arriving from the other side.
    let _guard = state.storage.lock_pack_config(&pack_id).await;
    // owner / tier / visibility / fork_of are server-controlled and never trusted
    // from the client. On an edit, carry them from the stored config so a member
    // can't reassign ownership, self-promote to official, or publish. On create,
    // derive them from the id's namespace: a community id (u/<uid>/...) is a draft
    // owned by that uid; a flat id is an official published operator pack.
    let fill = match state.storage.load_pack_config(&pack_id).await {
        Ok(existing) => {
            check_precondition(pre.as_ref(), Some(&rev_of(&existing)?), &pack_id)?;
            cfg.owner = existing.owner;
            cfg.tier = existing.tier;
            cfg.visibility = existing.visibility;
            cfg.fork_of = existing.fork_of.clone();
            // Sticky pulled dependencies: entries the fill appended on earlier
            // saves survive a body that lacks them (the client may never have
            // seen them), independent of whether the fill below can reach
            // Modrinth right now. Orphans are pruned by the fill itself once
            // nothing declared reaches them.
            crate::authoring::depfill::merge_pulled(&existing, &mut cfg);
            fill_needed(Some(&existing), &cfg)
        }
        Err(_) => {
            check_precondition(pre.as_ref(), None, &pack_id)?;
            match super::auth::pack_namespace_uid(&pack_id) {
                Some(uid) => {
                    super::auth::require_terms(&state, identity.uid).await?;
                    cfg.owner = uid;
                    cfg.tier = PackTier::Community;
                    cfg.visibility = Visibility::Draft;
                    cfg.fork_of = None;
                }
                None => {
                    cfg.owner = identity.uid;
                    cfg.tier = PackTier::Official;
                    cfg.visibility = Visibility::Published;
                    cfg.fork_of = None;
                }
            }
            true
        }
    };
    // A whole-config write is the other door into the same act, so it goes
    // through the same one. It also settles any live document: the document
    // would otherwise keep the version it remembers and put it back on the next
    // merge, quietly undoing this write.
    state.docs.forget(&pack_id);
    let (cfg, rev) = store_edited_config(&state, &pack_id, cfg, &identity.login, fill).await?;
    audit(
        &state,
        &identity,
        "pack.config",
        Some(&pack_id),
        Some(&format!("{} mods", cfg.mods.len())),
    )
    .await;
    Ok((StatusCode::CREATED, etag(&rev), Json(cfg)))
}

/// Whether the dependency fill has anything to do. It reaches Modrinth, so it
/// runs when the declared set changed and not when someone retitled the pack --
/// a document materialises every second or two while a person types, and a
/// network round trip per keystroke would be a denial of service we wrote
/// ourselves.
fn fill_needed(before: Option<&PackConfig>, after: &PackConfig) -> bool {
    // compared through their serialized form: the declarations are plain data,
    // and this needs no equality derive spreading across the domain types
    let declared = |c: &PackConfig| serde_json::to_value((&c.mods, &c.assets)).ok();
    before.is_none_or(|b| declared(b) != declared(after))
}

/// Store an edited config: write it, note the revision, and tell everyone in the
/// pack. Shared by the whole-config PUT and by the document's materialisation,
/// which are two doors into one act and must not drift.
///
/// The caller holds the pack's config lock and has already settled the
/// server-owned fields; `fill` says whether the dependency pass is worth its
/// network round trip.
async fn store_edited_config(
    state: &AppState,
    pack_id: &str,
    mut cfg: PackConfig,
    by: &str,
    fill: bool,
) -> Result<(PackConfig, String), ApiError> {
    // Pull in each mod's missing hard dependencies (Modrinth first, the mirror's
    // own cache second) and record the resolved requires graph, so the operator
    // never hand-manages libraries and the build can derive required-ness.
    // Best-effort: a Modrinth outage must not block saving a config, so a fill
    // error is logged and the raw config is saved.
    if fill {
        let cached: std::collections::HashSet<String> = state
            .storage
            .list_cache_inventory()
            .await
            .map(|inv| inv.into_iter().map(|e| e.sha1).collect())
            .unwrap_or_default();
        if let Err(e) = crate::authoring::depfill::fill_dependencies(
            &mut cfg,
            &state.registry,
            &state.modrinth,
            &cached,
        )
        .await
        {
            tracing::warn!(pack_id = %pack_id, error = %e, "dependency auto-fill failed; saving config as-is");
        }
    }
    state.storage.save_pack_config(pack_id, &cfg).await?;
    // Remember that this person worked, so the next commit names them without
    // anyone having to reconstruct it afterwards (#122). Best-effort: losing an
    // attribution is not a reason to fail a save the operator asked for.
    if let Err(e) = state.storage.note_pending_author(pack_id, by).await {
        tracing::warn!(pack_id = %pack_id, error = %e, "could not note the pending commit author");
    }
    // the revision of what was just stored, so the client that saved it edits
    // on from there without a re-read
    let rev = rev_of(&cfg)?;
    state.packs.publish(
        pack_id,
        crate::authoring::PackEvent::Saved {
            rev: rev.clone(),
            by: by.to_string(),
        },
    );
    Ok((cfg, rev))
}

#[derive(serde::Deserialize)]
struct RevertParams {
    version: String,
}

// Overwrite the authoring config with one reconstructed from a published build's
// manifest + summary -- the panel's "revert to build" affordance. Returns the new
// config so the editor can swap to it without a reload, and records the state it
// wrote as a checkpoint of its own: a revert is a decision somebody made, and one
// that leaves no trace in the history leaves the pack looking dirty for reasons
// nobody can read back.
async fn revert_pack_config(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Query(p): Query<RevertParams>,
) -> Result<([(HeaderName, String); 1], Json<PackConfig>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let manifest = state
        .storage
        .load_manifest_version(&pack_id, &p.version)
        .await?;
    let summary = state.storage.load_pack_summary(&pack_id).await?;
    let cfg = reconstruct_config(&manifest, &summary);
    // A revert is a deliberate overwrite of whatever is there, so it states no
    // precondition -- but it still takes the lock, so it cannot land in the
    // middle of a save's read-modify-write, and it answers with the revision it
    // produced so the editor keeps saving conditionally afterwards.
    let _guard = state.storage.lock_pack_config(&pack_id).await;
    // A live document remembers the config this replaces and would put it back
    // on the next merge, so the revert settles it too. Editors resync from the
    // reverted config, which is what a revert means to them.
    state.docs.forget(&pack_id);
    let (cfg, rev) = store_edited_config(&state, &pack_id, cfg, &identity.login, false).await?;
    // Recorded before the checkpoint: the config is already overwritten by this
    // point, and a checkpoint that fails must not take the record of the
    // overwrite with it.
    audit(
        &state,
        &identity,
        "pack.revert",
        Some(&pack_id),
        Some(&p.version),
    )
    .await;
    checkpoint(
        &state,
        &pack_id,
        &cfg,
        &identity,
        format!("revert to {}", p.version),
    )
    .await?;
    Ok((etag(&rev), Json(cfg)))
}

// ── pack history (#122) ──────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CommitReq {
    message: String,
}

/// Declare a checkpoint: snapshot the config as it stands, sign it, and name
/// everyone whose saved work it takes in.
///
/// Takes the config lock, so a commit cannot capture a half-applied
/// read-modify-write. It still captures someone's half-typed sentence -- the
/// state is shared and merges live, so there is no moment when everything is
/// quiet, and waiting for one is not a workable alternative.
async fn create_commit(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<CommitReq>,
) -> Result<(StatusCode, Json<crate::authoring::Commit>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return Err(ApiError::BadRequest(
            "a commit needs a message; an unlabelled checkpoint is one nobody can read later"
                .into(),
        ));
    }

    let _guard = state.storage.lock_pack_config(&pack_id).await;
    let cfg = state.storage.load_pack_config(&pack_id).await?;
    let parent = state.storage.commit_head(&pack_id).await;

    // The signer is a contributor to their own commit, and is named first --
    // they are the one who decided this state was worth keeping.
    let mut contributors = vec![identity.login.clone()];
    for who in state.storage.pending_authors(&pack_id).await {
        if !contributors.contains(&who) {
            contributors.push(who);
        }
    }

    let (commit, snapshot) = crate::authoring::make_commit(
        &cfg,
        parent,
        &identity.login,
        &message,
        contributors,
        crate::authoring::build::now_rfc3339(),
    )
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("commit encode: {e}")))?;

    state
        .storage
        .save_commit(&pack_id, &commit, &snapshot)
        .await?;
    state.packs.publish(
        &pack_id,
        crate::authoring::PackEvent::Committed {
            id: commit.id.clone(),
            by: identity.login.clone(),
            message: commit.message.clone(),
        },
    );
    audit(
        &state,
        &identity,
        "pack.commit",
        Some(&pack_id),
        Some(&commit.id),
    )
    .await;
    Ok((StatusCode::CREATED, Json(commit)))
}

/// One line of the log: the commit, and which published builds came out of it.
///
/// A checkpoint nobody shipped and one that is running on three hundred
/// machines look the same in a list of messages. The manifests already record
/// the commit they were built from; this is that record read back.
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "bindings/")]
pub struct CommitLogEntry {
    #[serde(flatten)]
    pub commit: crate::authoring::Commit,
    /// Pack versions built from this commit, newest first. Empty for a
    /// checkpoint that was never published.
    pub builds: Vec<String>,
}

/// How much history a caller that named no limit is answered with. The log was
/// capped before it could be paged, and a listing that suddenly walks a pack's
/// whole chain is a cost nobody asked for; naming a `limit` pages past it.
const DEFAULT_LOG_PAGE: usize = 100;

/// The log, newest first, paged by the chain itself: the cursor names the last
/// commit answered, and the next page starts at its parent. A pack whose history
/// outgrew one page was unreadable past its head before this -- the limit was
/// capped and there was nothing to continue from.
async fn list_commits(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Query(page): Query<PageQuery>,
    uri: Uri,
) -> Result<Response, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let after = page.cursor().and_then(|parts| parts.first().cloned());
    let log = state
        .storage
        .commit_log(
            &pack_id,
            after.as_deref(),
            page.probe().unwrap_or(DEFAULT_LOG_PAGE),
        )
        .await?;
    // One index for the whole log, and one that outlives this page: a pack that
    // has never published, or whose manifests cannot be listed, simply has no
    // builds to name -- the log is worth reading either way.
    let built = state.storage.builds_by_commit(&pack_id).await;
    let rows: Vec<CommitLogEntry> = log
        .into_iter()
        .map(|commit| CommitLogEntry {
            builds: built.get(&commit.id).cloned().unwrap_or_default(),
            commit,
        })
        .collect();
    Ok(page.answer(rows, &uri, |r| vec![r.commit.id.clone()]))
}

/// Where the history is and how far the working state has moved off it -- what
/// the panel needs to say "47 changes since the last commit" before a build.
async fn commit_status(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<crate::authoring::CommitStatus>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let live = state.storage.load_pack_config(&pack_id).await?;
    // Whether there is a checkpoint and whether its metadata reads are separate
    // questions: a commit whose metadata file is damaged is still a commit, and
    // answering "this pack has no history" would say the working state needs no
    // checkpoint while the build gate -- which reads `HEAD` -- refuses it.
    let head_id = state.storage.commit_head(&pack_id).await;
    let head = match &head_id {
        Some(id) => state.storage.load_commit(&pack_id, id).await.ok(),
        None => None,
    };
    let changes = match (&head_id, &head) {
        (None, _) => Vec::new(),
        // `config_rev` hashes a projection covering everything the diff reads,
        // so an equal revision means an empty diff and the snapshot need not be
        // read at all -- the everyday answer on a clean pack. The converse does
        // not hold, which is why this is a fast path and not the answer itself.
        (Some(_), Some(c)) if rev_of(&live).is_ok_and(|rev| rev == c.config_rev) => Vec::new(),
        (Some(id), _) => match state.storage.load_commit_config(&pack_id, id).await {
            Ok(snapshot) => crate::authoring::diff_configs(&snapshot, &live),
            Err(_) => vec![crate::authoring::whole_config()],
        },
    };
    Ok(Json(crate::authoring::CommitStatus {
        // A pack with no history has nothing to diff against, and one
        // outstanding change: the pack itself.
        uncommitted: if head_id.is_none() { 1 } else { changes.len() },
        changes,
        pending_authors: state.storage.pending_authors(&pack_id).await,
        head,
    }))
}

/// One commit by name, with the builds that came out of it.
///
/// A commit has an address, so the page behind that address must be readable
/// without the log that lists it -- the log is capped, and the checkpoint a
/// build names can be older than the cap.
async fn get_commit(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, commit_id)): Path<(String, String)>,
) -> Result<Json<CommitLogEntry>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let commit = state.storage.load_commit(&pack_id, &commit_id).await?;
    let builds = state
        .storage
        .builds_by_commit(&pack_id)
        .await
        .get(&commit.id)
        .cloned()
        .unwrap_or_default();
    Ok(Json(CommitLogEntry { commit, builds }))
}

async fn get_commit_config(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, commit_id)): Path<(String, String)>,
) -> Result<Json<PackConfig>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    Ok(Json(
        state
            .storage
            .load_commit_config(&pack_id, &commit_id)
            .await?,
    ))
}

#[derive(serde::Deserialize)]
struct DiffParams {
    /// What to read this commit against: another commit's id, or `live` for the
    /// working config. Absent -> the commit's parent, which is what this commit
    /// itself recorded.
    against: Option<String>,
}

/// Two states of one pack, and every difference between them.
#[derive(serde::Serialize, ts_rs::TS, utoipa::ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct CommitDiff {
    /// The left side: a commit id, the literal `live`, or absent on a first
    /// commit, which has nothing before it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from: Option<String>,
    /// The right side: always the commit named in the path.
    pub to: String,
    pub changes: Vec<crate::authoring::ConfigChange>,
}

/// What a commit recorded, or what separates it from any other state.
///
/// Reading a commit against `live` is the question a restore asks -- "what
/// happens if I put this back" -- which is why a restore can show its own
/// consequences before it is pressed rather than after.
async fn commit_diff(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, commit_id)): Path<(String, String)>,
    Query(p): Query<DiffParams>,
) -> Result<Json<CommitDiff>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let target = state
        .storage
        .load_commit_config(&pack_id, &commit_id)
        .await?;
    let commit = state.storage.load_commit(&pack_id, &commit_id).await?;
    let (from, before) = match p.against.as_deref() {
        Some("live") => (
            Some("live".to_string()),
            Some(state.storage.load_pack_config(&pack_id).await?),
        ),
        Some(other) => (
            Some(other.to_string()),
            Some(state.storage.load_commit_config(&pack_id, other).await?),
        ),
        None => match &commit.parent {
            Some(parent) => (
                Some(parent.clone()),
                Some(state.storage.load_commit_config(&pack_id, parent).await?),
            ),
            None => (None, None),
        },
    };
    let changes = match &before {
        Some(before) => crate::authoring::diff_configs(before, &target),
        None => crate::authoring::initial(&target),
    };
    Ok(Json(CommitDiff {
        from,
        to: commit_id,
        changes,
    }))
}

#[derive(serde::Deserialize)]
struct RestoreReq {
    /// What to call the commit this restore writes. Absent -> named after what
    /// it restores, which is the honest default.
    #[serde(default)]
    message: Option<String>,
}

/// Put a commit's state back, as a new commit.
///
/// History is append-only on purpose: restoring writes the old state forward
/// rather than moving `HEAD` backwards, so a restore is itself a decision
/// somebody made at a time, and nothing that was ever declared stops being
/// true. The alternative -- rewinding `HEAD` -- would make every build that
/// named a later commit refer to a history that no longer admits it.
async fn restore_commit(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, commit_id)): Path<(String, String)>,
    Json(req): Json<RestoreReq>,
) -> Result<(StatusCode, Json<crate::authoring::Commit>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let restored = state
        .storage
        .load_commit_config(&pack_id, &commit_id)
        .await?;

    let _guard = state.storage.lock_pack_config(&pack_id).await;
    // A live document remembers the config this replaces and would put it back
    // on the next merge, so the restore settles it too -- the same reason the
    // build-revert does.
    state.docs.forget(&pack_id);
    let (cfg, _rev) =
        store_edited_config(&state, &pack_id, restored, &identity.login, false).await?;

    let message = req
        .message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("restore {}", short_commit(&commit_id)));
    let commit = checkpoint(&state, &pack_id, &cfg, &identity, message).await?;
    audit(
        &state,
        &identity,
        "pack.commit.restore",
        Some(&pack_id),
        Some(&commit_id),
    )
    .await;
    Ok((StatusCode::CREATED, Json(commit)))
}

/// Write a state forward as a checkpoint: make the commit, store it, tell the
/// pack it happened.
///
/// The caller holds the config lock and has already written the config this
/// records. Shared by the two acts that put an older state back -- restoring a
/// commit, and reverting to a published build -- so neither can quietly stop
/// leaving a trace behind. Both name only the person who pressed it: they are
/// putting a state back, not signing off on whatever anyone else had in flight.
async fn checkpoint(
    state: &AppState,
    pack_id: &str,
    cfg: &PackConfig,
    identity: &Identity,
    message: String,
) -> Result<crate::authoring::Commit, ApiError> {
    let parent = state.storage.commit_head(pack_id).await;
    let (commit, snapshot) = crate::authoring::make_commit(
        cfg,
        parent,
        &identity.login,
        &message,
        vec![identity.login.clone()],
        crate::authoring::build::now_rfc3339(),
    )
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("commit encode: {e}")))?;
    state
        .storage
        .save_commit(pack_id, &commit, &snapshot)
        .await?;
    state.packs.publish(
        pack_id,
        crate::authoring::PackEvent::Committed {
            id: commit.id.clone(),
            by: identity.login.clone(),
            message: commit.message.clone(),
        },
    );
    Ok(commit)
}

/// The short form a person reads. Long enough to be unambiguous in a pack's
/// history, short enough to sit in a sentence.
fn short_commit(id: &str) -> &str {
    &id[..id.len().min(8)]
}

#[derive(serde::Deserialize)]
struct DuplicatePackReq {
    target_id: String,
    // Optional loader override, so a loader variant (e.g. a Cleanroom trial of a
    // Forge pack) is one call. Absent -> keep the source loader.
    #[serde(default)]
    loader: Option<LoaderSpec>,
}

// Clone a curated pack under a new id -- copy config + the per-pack static tree,
// with an optional loader override -- leaving the source untouched. The shared
// content-addressed cache means mod sources resolve without re-upload; the
// operator builds the new pack via the usual build endpoint. This is the
// single-admin primitive; ownership-aware forks (#36) layer on top of it.
async fn duplicate_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<DuplicatePackReq>,
) -> Result<(StatusCode, Json<PackConfig>), ApiError> {
    // reading the source is enough to copy it; the copy itself is written into
    // the target, which the caller must be able to edit
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    super::auth::authorize(&state, &identity, &req.target_id, PackLevel::Edit).await?;
    // the clone is owned by the target's namespace (a member's own uid), or by
    // the caller for an official (flat) target
    let owner = super::auth::pack_namespace_uid(&req.target_id).unwrap_or(identity.uid);
    let cfg = state
        .storage
        .duplicate_pack(&pack_id, &req.target_id, req.loader, owner, None)
        .await?;
    state.events.pack(&req.target_id, "created");
    Ok((StatusCode::CREATED, Json(cfg)))
}

/// Every pack summary, unfiltered -- the operator's view, including drafts,
/// unlisted, and community packs that the public `/v1/packs` listing hides.
///
/// Carries when each pack last built, like the two public listings: this is the
/// only listing the panel's own catalog reads, and without it a view that wants
/// to order packs by recency has nothing to order by but the version string,
/// which is a counter rather than a date.
async fn list_all_pack_summaries(
    State(state): State<AppState>,
) -> Result<Json<Vec<PackSummary>>, ApiError> {
    let mut summaries = state.storage.list_pack_summaries().await?;
    for summary in &mut summaries {
        super::public::enrich_latest_build(&state, summary).await;
    }
    Ok(Json(summaries))
}

#[derive(serde::Deserialize)]
struct VisibilityReq {
    visibility: Visibility,
}

/// Delete a pack and everything under it. Ownership-gated like the rest of
/// authoring: a member deletes only their own community packs.
async fn delete_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Own).await?;
    state.storage.delete_pack(&pack_id).await?;
    // The id can be minted again; its access list must not outlive the pack it
    // was written for.
    let acc = state.accounts.clone();
    let gone = pack_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        acc.forget_pack_access(&gone)?;
        acc.forget_pack_blocks(&gone)?;
        acc.forget_pack_threads(&gone)
    })
    .await;
    state.events.pack(&pack_id, "deleted");
    audit(&state, &identity, "pack.delete", Some(&pack_id), None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Publish / unpublish (or unlist) a pack. Takes effect on the public listing
/// immediately (see `Storage::set_pack_visibility`).
async fn set_pack_visibility(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<VisibilityReq>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Own).await?;
    state
        .storage
        .set_pack_visibility(&pack_id, req.visibility)
        .await?;
    state.events.pack(&pack_id, "visibility");
    audit(
        &state,
        &identity,
        "pack.visibility",
        Some(&pack_id),
        Some(&format!("{:?}", req.visibility)),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── access to a pack (ADR 0006) ─────────────────────────────────────────────

/// Who has been let into this pack, and at what level.
///
/// Only the granted rows: the owner of a community namespace and the admins are
/// not in the list because they are not in the table -- the view says so in
/// words rather than inventing rows the store does not have.
async fn list_access(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<Vec<crate::accounts::PackGrant>>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    let acc = state.accounts.clone();
    let pack = pack_id.clone();
    let rows = tokio::task::spawn_blocking(move || acc.list_pack_access(&pack))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("access task: {e}")))??;
    Ok(Json(rows))
}

#[derive(serde::Deserialize)]
struct GrantReq {
    /// GitHub uid of the person being let in.
    #[serde(alias = "uid")]
    github_uid: i64,
    /// `view` | `edit` | `own`.
    level: String,
}

/// Let somebody into this pack, or move the level they already have.
async fn grant_access(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<GrantReq>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Own).await?;
    let level = PackLevel::parse(&req.level)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown level {:?}", req.level)))?;
    // Granting to yourself is the one move that cannot mean anything: whoever
    // can grant already owns the pack.
    if req.github_uid == identity.uid {
        return Err(ApiError::BadRequest(
            "you already own this pack; a grant to yourself would say nothing".into(),
        ));
    }
    let acc = state.accounts.clone();
    let (pack, by) = (pack_id.clone(), identity.uid);
    let uid = req.github_uid;
    tokio::task::spawn_blocking(move || acc.grant_pack_access(&pack, uid, level, by))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("access task: {e}")))?
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    audit(
        &state,
        &identity,
        "pack.access.grant",
        Some(&pack_id),
        Some(&format!("{} -> {}", req.github_uid, level.as_str())),
    )
    .await;
    state.events.pack(&pack_id, "access");
    Ok(StatusCode::NO_CONTENT)
}

/// Take somebody's access away. A uid that had none is not an error: the state
/// asked for is the state that results either way.
async fn revoke_access(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, uid)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Own).await?;
    let acc = state.accounts.clone();
    let pack = pack_id.clone();
    let removed = tokio::task::spawn_blocking(move || {
        let removed = acc.revoke_pack_access(&pack, uid)?;
        // What they were told about this pack goes with the access: a
        // notification reads the thread's title live, so a list left behind
        // would keep showing a discussion they may no longer read.
        acc.forget_notifications_about(&pack, uid)?;
        Ok::<_, anyhow::Error>(removed)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("access task: {e}")))??;
    if removed {
        audit(
            &state,
            &identity,
            "pack.access.revoke",
            Some(&pack_id),
            Some(&uid.to_string()),
        )
        .await;
        state.events.pack(&pack_id, "access");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Serialize)]
struct MyLevel {
    /// The caller's level here, or absent when they have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<PackLevel>,
    /// Why they may not write, when they may not. A panel that only learns this
    /// from a refused request offers a reply box that cannot work and then
    /// explains itself in a toast; this is how it can say so before.
    #[serde(skip_serializing_if = "Option::is_none")]
    suspended: Option<Suspension>,
}

#[derive(serde::Serialize)]
struct Suspension {
    /// The words that were written, as the person stopped reads them.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    at: i64,
    /// Whether this is the mirror's own answer rather than one pack's, so the
    /// panel can say "here" or "anywhere" and mean it.
    everywhere: bool,
}

/// What the caller may do to this pack, answered by the same gate that enforces
/// it.
///
/// The panel used to guess this from the pack id and the caller's role, which
/// got two of the three answers right and hid merge, moderation and the block
/// list from everybody who reached a pack by grant -- the one case grants exist
/// for. Asking is one call when the editor opens, and it cannot drift from what
/// the writes will actually allow.
async fn my_level(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Json<MyLevel> {
    let mut level = None;
    for step in [PackLevel::Own, PackLevel::Edit, PackLevel::View] {
        if super::auth::may(&state, &identity, &pack_id, step).await {
            level = Some(step);
            break;
        }
    }
    // Whichever stop applies, with the account-wide one first: it is the more
    // serious, and it is true of every pack rather than this one.
    let acc = state.accounts.clone();
    let uid = identity.uid;
    let account = tokio::task::spawn_blocking(move || acc.suspension_of(uid))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let suspended = match account {
        Some(s) => Some(Suspension {
            reason: s.reason,
            at: s.at,
            everywhere: true,
        }),
        None => block_on(&state, &pack_id, identity.uid)
            .await
            .map(|b| Suspension {
                reason: b.reason,
                at: b.blocked_at,
                everywhere: false,
            }),
    };
    Json(MyLevel { level, suspended })
}

// ── blocking somebody from a pack's discussion ──────────────────────────────

#[derive(serde::Deserialize)]
struct BlockReq {
    #[serde(alias = "uid")]
    github_uid: i64,
    /// Why. Written for the person it names: they are shown it when a write of
    /// theirs is refused, and on their own page. A door that shuts with no word
    /// through it is one they can only argue with.
    #[serde(default)]
    reason: Option<String>,
}

/// Who a pack has stopped from writing on it. Readable by whoever moderates it,
/// because a moderator who cannot see the list cannot undo one of its rows.
async fn list_blocks(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
) -> Result<Json<Vec<crate::accounts::PackBlock>>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let acc = state.accounts.clone();
    let pack = pack_id.clone();
    let rows = tokio::task::spawn_blocking(move || acc.list_pack_blocks(&pack))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("block task: {e}")))??;
    Ok(Json(rows))
}

/// Stop somebody writing on this pack. Moderation-level, like hiding a comment:
/// the two are the same job, one answering what was said and one what would be
/// said next.
async fn block_from_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<BlockReq>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    // Blocking the people who keep the pack would be a way to lock them out of
    // their own discussion, so the gate refuses it rather than storing a row
    // that the write path would have to learn to ignore.
    if super::auth::may_uid(&state, &pack_id, req.github_uid, PackLevel::Edit).await {
        return Err(ApiError::BadRequest(
            "that person keeps this pack; take their access away first".into(),
        ));
    }
    let acc = state.accounts.clone();
    let (pack, by, uid) = (pack_id.clone(), identity.uid, req.github_uid);
    let reason = tidy_reason(req.reason.as_deref())?;
    let note = reason.clone();
    tokio::task::spawn_blocking(move || acc.block_from_pack(&pack, uid, note.as_deref(), by))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("block task: {e}")))?
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    audit(
        &state,
        &identity,
        "pack.block",
        Some(&pack_id),
        Some(&match &reason {
            Some(r) => format!("{uid}: {r}"),
            None => uid.to_string(),
        }),
    )
    .await;
    state.events.pack(&pack_id, "access");
    Ok(StatusCode::NO_CONTENT)
}

/// How long a block's reason may be. It is a label, not an essay: it rides in
/// every refusal that account gets and sits in a row of a list, so a reason that
/// needs more room than this is a comment on the thread, not a reason.
const MAX_BLOCK_REASON: usize = 300;

/// The reason as it will be read: one line, trimmed, and bounded.
///
/// Whitespace is collapsed rather than preserved because every place it appears
/// is a single line -- a notice where a reply box was, a row in the block list
/// -- and a reason with newlines in it would break both while looking fine to
/// whoever typed it.
fn tidy_reason(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(text) = raw else { return Ok(None) };
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return Ok(None);
    }
    if one_line.chars().count() > MAX_BLOCK_REASON {
        return Err(ApiError::BadRequest(format!(
            "that reason is longer than {MAX_BLOCK_REASON} characters; the thread is where the \
             long version goes"
        )));
    }
    Ok(Some(one_line))
}

/// Let somebody write here again.
async fn unblock_from_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path((pack_id, uid)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let acc = state.accounts.clone();
    let pack = pack_id.clone();
    let lifted = tokio::task::spawn_blocking(move || acc.unblock_from_pack(&pack, uid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("block task: {e}")))??;
    if lifted {
        audit(
            &state,
            &identity,
            "pack.unblock",
            Some(&pack_id),
            Some(&uid.to_string()),
        )
        .await;
        state.events.pack(&pack_id, "access");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Refuse a write from somebody who has been stopped, and say why.
///
/// Two answers, and both are needed: the mirror's operators decide who puts
/// anything on it at all, and a pack's keepers decide who writes in their
/// discussion. The account-wide one is checked first because it is the more
/// serious and the less local: being told about one pack's block while
/// suspended everywhere would be the wrong sentence.
///
/// The refusal carries the reason that was written: a door that shuts with no
/// word is one somebody can only argue with, and the panel has nothing to show
/// them but a generic failure.
///
/// A store that cannot be read stops nobody: a discussion that refuses comments
/// because sqlite hiccupped is a worse failure than one spam message.
async fn not_silenced(
    state: &AppState,
    identity: &Identity,
    pack_id: &str,
) -> Result<(), ApiError> {
    super::auth::not_suspended(state, identity).await?;
    let Some(block) = block_on(state, pack_id, identity.uid).await else {
        return Ok(());
    };
    Err(ApiError::Refused(match block.reason.as_deref() {
        Some(reason) => format!("writing here is suspended. reason: {reason}"),
        None => "writing here is suspended".into(),
    }))
}

/// This person's block on this pack, if there is one.
async fn block_on(state: &AppState, pack_id: &str, uid: i64) -> Option<crate::accounts::PackBlock> {
    let acc = state.accounts.clone();
    let pack = pack_id.to_string();
    tokio::task::spawn_blocking(move || acc.block_on(&pack, uid))
        .await
        .ok()?
        .ok()?
}

/// How much somebody may say in a window, before the mirror asks them to slow
/// down. Public writing without a ceiling is an invitation; these are set where
/// a person never notices them and a script does.
const COMMENTS_PER_WINDOW: i64 = 20;
const THREADS_PER_WINDOW: i64 = 5;
const RATE_WINDOW_SECS: i64 = 600;

/// Refuse a write that is the caller's twenty-first comment (or sixth thread) in
/// ten minutes. Counted from the rows themselves, so a restart hands nobody a
/// fresh allowance.
async fn within_rate(state: &AppState, kind: &'static str, uid: i64) -> Result<(), ApiError> {
    let acc = state.accounts.clone();
    let limit = if kind == "comments" {
        COMMENTS_PER_WINDOW
    } else {
        THREADS_PER_WINDOW
    };
    let recent = tokio::task::spawn_blocking(move || {
        let since = acc.now() - RATE_WINDOW_SECS;
        acc.recent_by(kind, uid, since)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("rate task: {e}")))??;
    if recent >= limit {
        return Err(ApiError::Conflict(
            "that is a lot in a short time; give it a few minutes".into(),
        ));
    }
    Ok(())
}

// ── threads: what is said about a pack (ADR 0006) ───────────────────────────

// ── telling people ──────────────────────────────────────────────────────────

/// Who is answerable for a pack, for telling them something happened on it.
///
/// The same people the gate would let act on it (ADR 0006): whoever the
/// namespace belongs to, or -- for an official pack, which has no namespace
/// owner -- the mirror's operators, plus anybody granted enough to act. Not the
/// `owner` field on the summary: that is a byline, and being told about a
/// discussion you cannot close would be worse than not being told.
async fn keepers_of(state: &AppState, pack_id: &str) -> Vec<i64> {
    let acc = state.accounts.clone();
    let pack = pack_id.to_string();
    let namespace = super::auth::pack_namespace_uid(pack_id);
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        match namespace {
            Some(uid) => out.push(uid),
            None => out.extend(acc.operator_uids().unwrap_or_default()),
        }
        if let Ok(grants) = acc.list_pack_access(&pack) {
            out.extend(
                grants
                    .iter()
                    .filter(|g| g.level >= PackLevel::Edit)
                    .map(|g| g.github_uid),
            );
        }
        out
    })
    .await
    .unwrap_or_default()
}

/// Everybody already in a discussion: whoever opened it and whoever has spoken.
async fn voices_in(state: &AppState, thread: &crate::accounts::Thread) -> Vec<i64> {
    let acc = state.accounts.clone();
    let id = thread.id;
    let mut out = vec![thread.by_uid];
    if let Ok(Ok(speakers)) = tokio::task::spawn_blocking(move || acc.speakers_on(id)).await {
        out.extend(speakers);
    }
    out
}

/// Tell people that a thread moved.
///
/// Never fatal: the comment was said and the proposal was decided whatever
/// happens here, and refusing the act because nobody could be told would be the
/// wrong way round.
async fn tell(state: &AppState, uids: Vec<i64>, kind: &'static str, thread_id: i64, actor: i64) {
    let acc = state.accounts.clone();
    if let Err(e) =
        tokio::task::spawn_blocking(move || acc.notify(&uids, kind, thread_id, actor)).await
    {
        tracing::warn!(error = %e, "could not write notifications");
    }
}

#[derive(serde::Deserialize)]
struct OpenIssueReq {
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(serde::Deserialize)]
struct OpenProposalReq {
    /// The fork the change lives in.
    source_pack: String,
    /// The commit being offered; absent means that fork's head.
    #[serde(default)]
    source_commit: Option<String>,
    title: String,
    #[serde(default)]
    body: String,
}

/// Report something about a pack.
///
/// A published pack takes reports from anyone signed in -- that is what makes a
/// report worth having. An unpublished one takes them from whoever can see it.
async fn open_issue(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<OpenIssueReq>,
) -> Result<(StatusCode, Json<crate::accounts::Thread>), ApiError> {
    if !is_published(&state, &pack_id).await {
        super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    }
    not_silenced(&state, &identity, &pack_id).await?;
    within_rate(&state, "threads", identity.uid).await?;
    let title = trimmed(&req.title, "an issue needs a title")?;
    let id = open_thread(
        &state, &identity, &pack_id, "issue", &title, &req.body, None,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(load_thread(&state, id).await?)))
}

/// Offer a fork's state back to the pack it came from.
///
/// The proposer needs to be able to edit what they are offering and to see what
/// they are offering it to. What is offered is a commit rather than "whatever
/// that fork says today": what a reviewer reads must not move while they read it.
async fn open_proposal(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Json(req): Json<OpenProposalReq>,
) -> Result<(StatusCode, Json<crate::accounts::Thread>), ApiError> {
    super::auth::authorize(&state, &identity, &req.source_pack, PackLevel::Edit).await?;
    if !is_published(&state, &pack_id).await {
        super::auth::authorize(&state, &identity, &pack_id, PackLevel::View).await?;
    }
    if req.source_pack == pack_id {
        return Err(ApiError::BadRequest(
            "a pack cannot propose to itself; commit instead".into(),
        ));
    }
    not_silenced(&state, &identity, &pack_id).await?;
    within_rate(&state, "threads", identity.uid).await?;
    let title = trimmed(&req.title, "a proposal needs a title")?;
    let commit = match req.source_commit {
        Some(id) => state.storage.load_commit(&req.source_pack, &id).await?.id,
        None => state
            .storage
            .commit_head(&req.source_pack)
            .await
            .ok_or_else(|| {
                ApiError::Conflict(
                    "that pack has no commits yet, and a proposal offers one -- declare a \
                     checkpoint first"
                        .into(),
                )
            })?,
    };
    let id = open_thread(
        &state,
        &identity,
        &pack_id,
        "proposal",
        &title,
        &req.body,
        Some((req.source_pack.as_str(), commit.as_str())),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(load_thread(&state, id).await?)))
}

fn trimmed(value: &str, complaint: &str) -> Result<String, ApiError> {
    let t = value.trim().to_string();
    if t.is_empty() {
        return Err(ApiError::BadRequest(complaint.into()));
    }
    Ok(t)
}

async fn open_thread(
    state: &AppState,
    identity: &Identity,
    pack_id: &str,
    kind: &str,
    title: &str,
    body: &str,
    source: Option<(&str, &str)>,
) -> Result<i64, ApiError> {
    let acc = state.accounts.clone();
    let (pack, k, t, b, by) = (
        pack_id.to_string(),
        kind.to_string(),
        title.to_string(),
        body.trim().to_string(),
        identity.uid,
    );
    let source = source.map(|(p, c)| (p.to_string(), c.to_string()));
    let id = tokio::task::spawn_blocking(move || {
        let src = source.as_ref().map(|(p, c)| (p.as_str(), c.as_str()));
        acc.open_thread(&pack, &k, &t, &b, by, src)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    audit(
        state,
        identity,
        &format!("pack.{kind}.open"),
        Some(pack_id),
        Some(&format!("#{id} {title}")),
    )
    .await;
    tell(
        state,
        keepers_of(state, pack_id).await,
        "opened",
        id,
        identity.uid,
    )
    .await;
    state.events.pack(pack_id, "thread");
    Ok(id)
}

/// Whether a pack is published, which is what makes it reportable and
/// proposable to by somebody who was never let into it.
async fn is_published(state: &AppState, pack_id: &str) -> bool {
    state
        .storage
        .load_pack_summary(pack_id)
        .await
        .map(|s| s.visibility == Visibility::Published)
        .unwrap_or(false)
}

async fn load_thread(state: &AppState, id: i64) -> Result<crate::accounts::Thread, ApiError> {
    let acc = state.accounts.clone();
    tokio::task::spawn_blocking(move || acc.thread(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??
        .ok_or(ApiError::NotFound)
}

/// May this caller read this pack's threads at all? A published pack's
/// discussion is public; an unpublished one's is for whoever can see the pack.
async fn may_read_threads(state: &AppState, identity: &Identity, pack_id: &str) -> bool {
    is_published(state, pack_id).await
        || super::auth::may(state, identity, pack_id, PackLevel::View).await
}

#[derive(serde::Deserialize)]
struct CommentReq {
    body: String,
}

/// Say something on a thread. Anyone who can read the discussion can join it.
async fn post_comment(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Json(req): Json<CommentReq>,
) -> Result<(StatusCode, Json<crate::accounts::ThreadComment>), ApiError> {
    let t = load_thread(&state, id).await?;
    if !may_read_threads(&state, &identity, &t.pack_id).await {
        return Err(ApiError::Forbidden);
    }
    not_silenced(&state, &identity, &t.pack_id).await?;
    within_rate(&state, "comments", identity.uid).await?;
    let body = trimmed(&req.body, "an empty comment says nothing")?;
    if body.chars().count() > 4000 {
        return Err(ApiError::BadRequest("that comment is too long".into()));
    }
    let acc = state.accounts.clone();
    let (by, text) = (identity.uid, body.clone());
    let cid = tokio::task::spawn_blocking(move || acc.comment(id, by, &text))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("comment task: {e}")))??;
    let mut audience = voices_in(&state, &t).await;
    audience.extend(keepers_of(&state, &t.pack_id).await);
    tell(&state, audience, "comment", id, identity.uid).await;
    state.events.pack(&t.pack_id, "thread");
    let acc = state.accounts.clone();
    let mine = tokio::task::spawn_blocking(move || acc.comment_by_id(cid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("comment task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(mine)))
}

#[derive(serde::Deserialize)]
struct HideReq {
    #[serde(default = "yes")]
    hidden: bool,
}

fn yes() -> bool {
    true
}

/// Take a comment down, or put it back. The pack's keepers moderate their own
/// discussion; an admin moderates anywhere.
async fn moderate_comment(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(comment_id): Path<i64>,
    Json(req): Json<HideReq>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let thread_id = tokio::task::spawn_blocking(move || acc.thread_of_comment(comment_id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("comment task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    let t = load_thread(&state, thread_id).await?;
    super::auth::authorize(&state, &identity, &t.pack_id, PackLevel::Edit).await?;
    let acc = state.accounts.clone();
    let by = identity.uid;
    let hidden = req.hidden;
    let changed =
        tokio::task::spawn_blocking(move || acc.set_comment_hidden(comment_id, hidden, by))
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("comment task: {e}")))??;
    if !changed {
        return Err(ApiError::NotFound);
    }
    audit(
        &state,
        &identity,
        if hidden {
            "pack.comment.hide"
        } else {
            "pack.comment.show"
        },
        Some(&t.pack_id),
        Some(&format!("comment {comment_id}")),
    )
    .await;
    state.events.pack(&t.pack_id, "thread");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct DecideReq {
    /// What a merge commit should say; absent names the proposal.
    #[serde(default)]
    message: Option<String>,
}

/// Take a proposal: write its authored content into the target as a commit.
///
/// The server-controlled fields stay the target's -- ownership does not travel
/// with a proposal, or a fork could rename its way into a takeover.
async fn merge_proposal(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
    Json(req): Json<DecideReq>,
) -> Result<Json<crate::accounts::Thread>, ApiError> {
    let t = load_thread(&state, id).await?;
    super::auth::authorize(&state, &identity, &t.pack_id, PackLevel::Edit).await?;

    // Taking a proposal writes a config, a commit, and the decision itself, and
    // those three are one act. The thread is re-read inside the pack lock
    // because the status read before it decides nothing: two reviewers pressing
    // merge both pass it, the second waits here, and -- without this -- goes on
    // to write a second merge commit of an already-merged proposal before
    // `settle_thread` finally tells it the decision was made. The pack gets one
    // decision and one commit; the loser is refused before writing anything.
    let _guard = state.storage.lock_pack_config(&t.pack_id).await;
    let t = load_thread(&state, id).await?;
    if t.status != "open" {
        return Err(ApiError::Conflict(format!(
            "this proposal is already {}",
            t.status
        )));
    }
    let (Some(source_pack), Some(source_commit)) = (&t.source_pack, &t.source_commit) else {
        return Err(ApiError::BadRequest("this thread offers no state".into()));
    };
    let offered = state
        .storage
        .load_commit_config(source_pack, source_commit)
        .await?;
    let current = state.storage.load_pack_config(&t.pack_id).await?;
    let merged = current.with_authored_from(&offered);
    if let Some(dup) = merged.duplicate_declaration() {
        return Err(ApiError::BadRequest(dup));
    }
    state.docs.forget(&t.pack_id);
    let (cfg, _rev) =
        store_edited_config(&state, &t.pack_id, merged, &identity.login, false).await?;
    let message = req
        .message
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("merge #{id} from {source_pack}"));
    let commit = checkpoint(&state, &t.pack_id, &cfg, &identity, message).await?;

    let acc = state.accounts.clone();
    let (by, cid) = (identity.uid, commit.id.clone());
    let settled =
        tokio::task::spawn_blocking(move || acc.settle_thread(id, "merged", by, Some(&cid)))
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    if !settled {
        return Err(ApiError::Conflict(
            "this proposal was already settled".into(),
        ));
    }
    audit(
        &state,
        &identity,
        "pack.proposal.merge",
        Some(&t.pack_id),
        Some(&format!("#{id} as {}", commit.id)),
    )
    .await;
    tell(
        &state,
        voices_in(&state, &t).await,
        "settled",
        id,
        identity.uid,
    )
    .await;
    state.events.pack(&t.pack_id, "thread");
    Ok(Json(load_thread(&state, id).await?))
}

/// Close an issue, decline a proposal, or take back your own of either.
async fn close_thread(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
) -> Result<Json<crate::accounts::Thread>, ApiError> {
    let t = load_thread(&state, id).await?;
    let mine = t.by_uid == identity.uid;
    if !mine {
        super::auth::authorize(&state, &identity, &t.pack_id, PackLevel::Edit).await?;
    }
    let status = match (t.kind.as_str(), mine) {
        ("proposal", true) => "withdrawn",
        ("proposal", false) => "declined",
        _ => "closed",
    };
    let acc = state.accounts.clone();
    let by = identity.uid;
    let settled = tokio::task::spawn_blocking(move || acc.settle_thread(id, status, by, None))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    if !settled {
        return Err(ApiError::Conflict("this thread was already settled".into()));
    }
    audit(
        &state,
        &identity,
        &format!("pack.{}.{status}", t.kind),
        Some(&t.pack_id),
        Some(&format!("#{id}")),
    )
    .await;
    tell(
        &state,
        voices_in(&state, &t).await,
        "settled",
        id,
        identity.uid,
    )
    .await;
    state.events.pack(&t.pack_id, "thread");
    Ok(Json(load_thread(&state, id).await?))
}

/// Reopen a closed issue. A proposal does not reopen: its offer was a commit,
/// and offering again is a new proposal rather than a resurrected one.
async fn reopen_thread(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<i64>,
) -> Result<Json<crate::accounts::Thread>, ApiError> {
    let t = load_thread(&state, id).await?;
    if t.by_uid != identity.uid {
        super::auth::authorize(&state, &identity, &t.pack_id, PackLevel::Edit).await?;
    }
    let acc = state.accounts.clone();
    let reopened = tokio::task::spawn_blocking(move || acc.reopen_issue(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("thread task: {e}")))??;
    if !reopened {
        return Err(ApiError::Conflict(
            "only a closed issue reopens; a proposal is offered again as a new one".into(),
        ));
    }
    tell(
        &state,
        voices_in(&state, &t).await,
        "settled",
        id,
        identity.uid,
    )
    .await;
    state.events.pack(&t.pack_id, "thread");
    Ok(Json(load_thread(&state, id).await?))
}

// ── helpers ────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct PutCacheResponse {
    schema_version: u32,
    sha1: String,
    size_bytes: u64,
}

#[derive(serde::Serialize)]
struct PutStaticResponse {
    schema_version: u32,
    pack_id: String,
    rel_path: String,
    size_bytes: u64,
}

#[derive(serde::Serialize)]
struct StaticListing {
    schema_version: u32,
    pack_id: String,
    files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_BLOCK_REASON, Precondition, check_precondition, fill_needed, github_asset_url,
        precondition, tidy_reason,
    };
    use crate::domain::{DeclaredMod, LoaderSpec, PackConfig, PackTier, SourceDecl, Visibility};
    use axum::http::{HeaderMap, HeaderValue, header};

    // Whatever a keeper types is what somebody else reads on their own screen,
    // in a place with room for one line.
    #[test]
    fn a_reason_is_one_bounded_line_or_nothing() {
        assert_eq!(tidy_reason(None).unwrap(), None);
        assert_eq!(
            tidy_reason(Some("   ")).unwrap(),
            None,
            "blank says nothing"
        );
        assert_eq!(
            tidy_reason(Some("  ПЕТУХ  ")).unwrap().as_deref(),
            Some("ПЕТУХ")
        );
        assert_eq!(
            tidy_reason(Some("flooding\n\nthree threads\tin a minute"))
                .unwrap()
                .as_deref(),
            Some("flooding three threads in a minute"),
            "a notice and a table row are both one line"
        );
        // counted in characters, not bytes: a Cyrillic reason is not half as
        // long as a Latin one
        let long = "я".repeat(MAX_BLOCK_REASON);
        assert!(tidy_reason(Some(&long)).is_ok());
        assert!(tidy_reason(Some(&"я".repeat(MAX_BLOCK_REASON + 1))).is_err());
    }

    fn headers(if_match: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::IF_MATCH, HeaderValue::from_str(if_match).unwrap());
        h
    }

    fn cfg(mods: Vec<DeclaredMod>) -> PackConfig {
        PackConfig {
            pack_id: "Industrial".into(),
            display_name: "Industrial".into(),
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

    fn declared(filename: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache {
                sha1: "a".repeat(40),
            },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    // The dependency fill reaches Modrinth. A document materialises every second
    // or two while somebody types, so running the fill on every write would be a
    // network round trip per sentence -- and the fill has nothing to do unless
    // the declared set actually moved.
    #[test]
    fn the_dependency_fill_runs_on_a_changed_declaration_and_not_on_prose() {
        let before = cfg(vec![declared("jei.jar")]);

        let mut retitled = before.clone();
        retitled.display_name = "Industrial II".into();
        retitled.tagline = "now with more".into();
        assert!(
            !fill_needed(Some(&before), &retitled),
            "renaming a pack pulls no dependencies"
        );

        let mut added = before.clone();
        added.mods.push(declared("ae2.jar"));
        assert!(
            fill_needed(Some(&before), &added),
            "a new mod might need one"
        );

        assert!(fill_needed(None, &before), "a pack being created is filled");
    }

    // A server is recorded as a host or a host:port, and Minecraft's default is
    // the mirror's to know rather than the operator's to type. A port that is
    // not a port is not an address -- silently falling back to 25565 would ask
    // the wrong server and report its list as this one's.
    #[test]
    fn an_address_is_a_host_and_optionally_a_port() {
        use super::split_address;
        assert_eq!(
            split_address("mc.example.com"),
            Some(("mc.example.com".to_string(), 25565))
        );
        assert_eq!(
            split_address("  mc.example.com:25566 "),
            Some(("mc.example.com".to_string(), 25566))
        );
        assert_eq!(split_address(""), None);
        assert_eq!(split_address("   "), None);
        assert_eq!(split_address("mc.example.com:hello"), None);
    }

    #[test]
    fn if_match_reads_quoted_weak_and_wildcard_tags() {
        // what the panel echoes back from an ETag, and what a hand-written
        // request is likely to carry, must mean the same revision
        for raw in ["\"abc123\"", "abc123", "W/\"abc123\"", "  \"abc123\" "] {
            match precondition(&headers(raw)) {
                Some(Precondition::Rev(rev)) => assert_eq!(rev, "abc123", "raw {raw:?}"),
                _ => panic!("expected a revision for {raw:?}"),
            }
        }
        assert!(matches!(
            precondition(&headers("*")),
            Some(Precondition::Any)
        ));
        assert!(precondition(&HeaderMap::new()).is_none(), "header absent");
        assert!(precondition(&headers("\"\"")).is_none(), "empty tag");
    }

    #[test]
    fn a_stale_base_is_refused_and_a_current_one_passes() {
        let stale = Precondition::Rev("aaaa1111".into());
        let current = Precondition::Rev("bbbb2222".into());

        assert!(check_precondition(None, Some("bbbb2222"), "Industrial").is_ok());
        assert!(check_precondition(Some(&current), Some("bbbb2222"), "Industrial").is_ok());
        assert!(
            check_precondition(Some(&Precondition::Any), Some("bbbb2222"), "Industrial").is_ok()
        );

        let err = check_precondition(Some(&stale), Some("bbbb2222"), "Industrial")
            .expect_err("a base that no longer matches disk is a conflict");
        let msg = err.to_string();
        assert!(msg.contains("Industrial"), "names the pack: {msg}");
        assert!(
            msg.contains("aaaa1111") && msg.contains("bbbb2222"),
            "{msg}"
        );

        // a config that is gone answers a conditional write with a conflict,
        // not a silent create over the top of a delete
        assert!(check_precondition(Some(&stale), None, "Industrial").is_err());
        assert!(check_precondition(Some(&Precondition::Any), None, "Industrial").is_err());
        assert!(
            check_precondition(None, None, "Industrial").is_ok(),
            "an unconditional first save still creates"
        );
    }

    #[test]
    fn github_url_accepts_plain_repo_and_simple_names() {
        assert_eq!(
            github_asset_url("Kitty-Hivens/open-smrt-network", "v1.2.3", "osn-1.12.2.jar"),
            Some(
                "https://github.com/Kitty-Hivens/open-smrt-network/releases/download/v1.2.3/osn-1.12.2.jar"
                    .into()
            )
        );
    }

    #[test]
    fn github_url_normalizes_a_pasted_url() {
        // a pasted browser URL (scheme + host, trailing .git/slash) still resolves
        for repo in [
            "https://github.com/owner/repo",
            "github.com/owner/repo/",
            "owner/repo.git",
        ] {
            assert_eq!(
                github_asset_url(repo, "v1", "a.jar"),
                Some("https://github.com/owner/repo/releases/download/v1/a.jar".into()),
                "repo {repo:?}"
            );
        }
    }

    #[test]
    fn github_url_percent_encodes_rich_asset_names() {
        // spaces / parens / plus -- common in real release assets -- are encoded,
        // not rejected
        let url = github_asset_url("o/r", "1.0+build5", "Cool Mod (1.12.2).jar").unwrap();
        assert_eq!(
            url,
            "https://github.com/o/r/releases/download/1.0%2Bbuild5/Cool%20Mod%20(1.12.2).jar"
        );
    }

    #[test]
    fn github_url_rejects_unsafe_inputs() {
        // repo must be exactly owner/name
        assert!(github_asset_url("owner/repo/extra", "v1", "a.jar").is_none());
        assert!(github_asset_url("justowner", "v1", "a.jar").is_none());
        // tag/asset can't add path depth or traverse
        assert!(github_asset_url("o/r", "v1/x", "a.jar").is_none());
        assert!(github_asset_url("o/r", "v1", "sub/a.jar").is_none());
        assert!(github_asset_url("o/r", "..", "a.jar").is_none());
        assert!(github_asset_url("o/r", "v1", "").is_none());
    }
}
