//! Member-accessible API: endpoints any signed-in user may call, gated by
//! `require_session` and scoped to what they own. Distinct from `admin`, which
//! requires the admin role -- this is the member tier of the ladder.

use super::{ApiError, audit};
use crate::accounts::{Identity, PackLevel, Role, UploadRow};
use crate::authoring::curator::{clean_mc_version, jar_facts, read_mcmod_info};
use crate::authoring::modmeta;
use crate::domain::{PackConfig, PackSummary, PackTier, Visibility};
use crate::registry::queries;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use super::MAX_UPLOAD_BODY;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/me/packs", get(my_packs))
        .route("/v1/me/authoring", get(my_authoring))
        .route("/v1/me/packs/{pack_id}/uploads", post(upload_jar))
        .route("/v1/me/uploads", get(my_uploads))
        .route("/v1/me/forks", post(fork_pack))
        .route("/v1/me/accept-terms", post(accept_terms))
        .route("/v1/me/notifications", get(my_notifications))
        .route("/v1/me/notifications/read", post(read_notifications))
        .route("/v1/me/feed-key", get(my_feed_key).post(rotate_my_feed_key))
        .route("/v1/events", get(mirror_events))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY))
        .layer(from_fn_with_state(
            state.clone(),
            super::auth::require_session,
        ))
        .with_state(state)
}

/// What is changing on the mirror, as it changes: the registry index moving, a
/// pack publishing, the moderation queue filling.
///
/// A view listens here instead of asking again on a timer. Polling is both
/// dearer and slower -- dearer because every view pays for a round trip whether
/// or not anything happened, slower because whatever happens is invisible until
/// the next tick. What arrives is a nudge rather than the data: the client
/// refetches the one view that cares, and that read is the same conditional GET
/// as before, usually answered `304`.
///
/// Session-gated, and filtered again per event: everything the panel already
/// shows a member is theirs to hear about, while the moderation queue is the
/// operator's.
async fn mirror_events(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use axum::response::sse::{Event, KeepAlive, Sse};

    let mut subscription = state.events.subscribe(identity.role);
    let stream = async_stream::stream! {
        while let Some(event) = subscription.next().await {
            let name = event.name();
            match serde_json::to_string(&event) {
                Ok(data) => yield Ok::<Event, std::convert::Infallible>(
                    Event::default().event(name).data(data),
                ),
                // an event that will not serialise is this process's bug, not
                // the subscriber's problem: drop it and keep the stream alive
                Err(e) => tracing::warn!(error = %e, "dropping an unserialisable mirror event"),
            }
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

/// Whether a pack the caller owns belongs in their own list. An admin curates
/// the official packs in a section of their own, and the operator owns every one
/// of those, so for an admin "mine" means the personal ones. A member has no
/// such section: a pack of theirs made official stays in their list, or they
/// would lose the way into a pack the gate still lets them edit.
fn is_own_listing(identity: &Identity, owner: i64, tier: PackTier) -> bool {
    owner == identity.uid && (tier == PackTier::Community || identity.role < Role::Admin)
}

/// The caller's own packs -- the "my packs" view. Draft and community packs the
/// public `/v1/packs` listing hides show here for their owner.
async fn my_packs(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<Vec<PackSummary>>, ApiError> {
    let mine = state
        .storage
        .list_pack_summaries()
        .await?
        .into_iter()
        .filter(|p| is_own_listing(&identity, p.owner, p.tier))
        .collect();
    Ok(Json(mine))
}

/// The caller's own authoring pack ids, including unbuilt drafts that have no
/// summary yet. The "my packs" list unions this with the built summaries so a
/// freshly-created draft is reachable before its first build.
async fn my_authoring(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<Vec<String>>, ApiError> {
    let all = state.storage.list_authoring_packs().await?;
    if identity.role < Role::Admin {
        // A grant is a row, so "which packs can I reach" is not derivable from
        // the id alone -- but it is still one question, asked once against the
        // caller's whole access list rather than once per pack on the mirror.
        return Ok(Json(
            super::auth::filter_may(&state, &identity, all, PackLevel::View).await,
        ));
    }
    // The admin rung reaches every pack, which is not the question asked here:
    // the stored owner and tier say which are the admin's own.
    let mut out = Vec::new();
    for id in all {
        if let Ok(cfg) = state.storage.load_pack_config(&id).await
            && is_own_listing(&identity, cfg.owner, cfg.tier)
        {
            out.push(id);
        }
    }
    Ok(Json(out))
}

// ── being told ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct NotificationParams {
    /// `?unread=true` is what a reader usually wants; absent reads everything.
    #[serde(default)]
    unread: bool,
    /// A list that only grows is read a bounded slice at a time.
    limit: Option<usize>,
}

#[derive(serde::Serialize)]
struct NotificationListing {
    /// How many are unread in total, which is not the same as how many unread
    /// rows are in this answer -- the badge counts the first, the list shows the
    /// second.
    unread: i64,
    rows: Vec<crate::accounts::Notification>,
}

/// What happened on the discussions this caller is part of: their reports
/// answered, their proposals decided, and threads opened on packs they keep.
///
/// Their own acts are not in it: being told what you just did is noise, and the
/// list exists to say what you have not seen.
async fn my_notifications(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(p): Query<NotificationParams>,
    Query(page): Query<super::page::PageQuery>,
    uri: axum::http::Uri,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let acc = state.accounts.clone();
    let uid = identity.uid;
    let after = page
        .cursor()
        .and_then(|parts| parts.first()?.parse::<i64>().ok());
    // `limit` here is the paging one; the plain cap stays for a caller that
    // wants a bounded answer and no cursor to follow.
    let probe = page.probe().or(p.limit.map(|n| n.clamp(1, 200)));
    let listing = tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>(NotificationListing {
            unread: acc.unread_count(uid)?,
            rows: acc.notifications_for(uid, p.unread, after, probe)?,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("notification task: {e}")))??;
    let (rows, next) = page.split(listing.rows, |n| vec![n.id.to_string()]);
    let mut resp = Json(NotificationListing {
        unread: listing.unread,
        rows,
    })
    .into_response();
    if let Some(value) = super::page::next_link(&uri, next.as_deref()) {
        resp.headers_mut().insert(axum::http::header::LINK, value);
    }
    Ok(resp)
}

#[derive(serde::Serialize)]
struct FeedKey {
    /// The whole address, not just the key: what somebody pastes into a reader.
    url: String,
}

/// Where this account's notifications can be read from outside the panel.
///
/// Minted on first ask rather than at sign-up: an account that never wants a
/// feed never has a key to leak.
async fn my_feed_key(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<FeedKey>, ApiError> {
    feed_key(&state, identity.uid, false).await
}

/// Retire the address and mint another, for a key that got out.
async fn rotate_my_feed_key(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<FeedKey>, ApiError> {
    feed_key(&state, identity.uid, true).await
}

async fn feed_key(state: &AppState, uid: i64, rotate: bool) -> Result<Json<FeedKey>, ApiError> {
    let acc = state.accounts.clone();
    let key = tokio::task::spawn_blocking(move || {
        if rotate {
            acc.rotate_feed_key(uid)
        } else {
            acc.feed_key(uid)
        }
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("feed key task: {e}")))?
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(FeedKey {
        url: format!(
            "{}/v1/feed.atom?key={key}",
            state.config.mirror_base.trim_end_matches('/')
        ),
    }))
}

#[derive(Deserialize)]
struct ReadReq {
    /// One of them; absent marks everything read.
    #[serde(default)]
    id: Option<i64>,
}

/// Mark one as read, or all of them. Scoped to the caller: an id is not a
/// capability, so this can only ever mark the caller's own.
async fn read_notifications(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<ReadReq>,
) -> Result<StatusCode, ApiError> {
    let acc = state.accounts.clone();
    let uid = identity.uid;
    tokio::task::spawn_blocking(move || acc.mark_read(uid, req.id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("notification task: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UploadParams {
    filename: String,
    /// Who the uploader names as the jar's upstream origin -- archival provenance.
    #[serde(default)]
    maintainer: Option<String>,
    /// Force past the Modrinth-coverage gate (a repackaged/relabeled jar for a
    /// foreign FML handshake). A debug operation (#39); ignored for lesser roles.
    #[serde(default)]
    force: bool,
}

/// Upload a self-hosted jar for one of the caller's community packs. Two auto-
/// gates enforce "self-host archival only": a jar whose sha1 Modrinth already
/// serves is the genuine file (rejected), and a Modrinth-known mod whose
/// (mc, loader) target Modrinth already carries is a relabel (rejected). Anything
/// else stages under `uploads/` and enters the moderation queue as `pending`; an
/// operator promotes it to the shared cache on approval. See the upload-moderation
/// policy.
async fn upload_jar(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Query(p): Query<UploadParams>,
    body: Bytes,
) -> Result<(StatusCode, Json<UploadRow>), ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let sha1 = crate::storage::sha1_hex(&body);

    // Auto-gate: a jar Modrinth already serves is the genuine file, not archival.
    let known = state
        .modrinth
        .version_files_by_sha1(std::slice::from_ref(&sha1))
        .await
        .map_err(ApiError::Internal)?;
    if known.contains_key(&sha1) {
        return Err(ApiError::BadRequest(
            "this jar is on Modrinth -- add it via the Modrinth picker, not a self-hosted upload"
                .into(),
        ));
    }

    // Forcing past the coverage gate is the debug-only escape hatch (#39): a
    // repackaged/relabeled jar hosted to satisfy a foreign server's FML handshake
    // (#37). A non-debug caller cannot request it.
    if p.force && identity.role < Role::Debug {
        return Err(ApiError::Forbidden);
    }
    let forcing = p.force && identity.role >= Role::Debug;

    // Coverage gate: a Modrinth-known mod whose (mc, loader) target Modrinth
    // already carries is a relabel, not archival. Conservative -- any uncertainty
    // falls through to the human moderation queue. Skipped only for a debug force.
    if !forcing && let Some(reason) = modrinth_covers_upload(&state, &body).await? {
        return Err(ApiError::BadRequest(reason));
    }

    state.storage.stage_upload(&sha1, &body).await?;

    let uid = identity.uid;
    let size = body.len() as i64;
    let maintainer = p.maintainer.filter(|s| !s.trim().is_empty());
    let (audit_sha, audit_pack) = (sha1.clone(), pack_id.clone());
    let (acc, pid, fname, sha, m) = (
        state.accounts.clone(),
        pack_id,
        p.filename,
        sha1,
        maintainer,
    );
    let id = tokio::task::spawn_blocking(move || {
        acc.enqueue_upload(uid, &pid, &fname, &sha, size, m.as_deref())
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("enqueue task: {e}")))??;

    // A forced upload bypassed a policy gate -- record it (the normal queued path
    // is already visible to moderators, so it needs no audit line of its own).
    if forcing {
        audit(
            &state,
            &identity,
            "upload.force",
            Some(&audit_sha),
            Some(&audit_pack),
        )
        .await;
    }

    state.events.moderation("queued");

    let acc = state.accounts.clone();
    let row = tokio::task::spawn_blocking(move || acc.get_upload(id))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("read upload task: {e}")))??
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// The identity facts the coverage gate reads from a jar: its modid, loader, and
/// declared Minecraft version. `None` if any is undeterminable -- not enough to
/// judge coverage, so the caller lets it through to the human queue.
fn extract_upload_facts(body: &[u8]) -> Option<(String, String, String)> {
    let loader = jar_facts(body).loader?;
    let mcmod = read_mcmod_info(body).ok().flatten();
    let meta = modmeta::read_mod_meta(body);
    let modid = mcmod
        .as_ref()
        .map(|i| i.modid.clone())
        .filter(|s| !s.is_empty())
        .or(meta.modid)?;
    let mc = mcmod
        .as_ref()
        .and_then(|i| clean_mc_version(&i.mcversion))
        .or(meta.mc)?;
    Some((modid, loader, mc))
}

/// Whether Modrinth already carries this upload's mod for its (mc, loader) target
/// -- in which case it is a relabelled counterfeit, not archival. Registry-based:
/// the modid resolves to a Modrinth project only if we have already harvested that
/// mod with its Modrinth identity. `Ok(None)` (let a human decide) whenever the
/// mod is not Modrinth-known here, its facts are undeterminable, or Modrinth has
/// the mod but not this target. Returns the rejection message when covered.
async fn modrinth_covers_upload(state: &AppState, body: &[u8]) -> Result<Option<String>, ApiError> {
    let Some((modid, loader, mc)) = extract_upload_facts(body) else {
        return Ok(None);
    };
    // modid -> our mod -> its Modrinth project id (a blocking DB read)
    let registry = state.registry.clone();
    let key = modid.clone();
    let project = tokio::task::spawn_blocking(move || {
        registry.with_conn(|c| {
            let Some(mod_id) = queries::mod_id_for_alias(c, "modid", &key)? else {
                return Ok(None);
            };
            queries::modrinth_id_for_mod(c, mod_id)
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("registry lookup task: {e}")))?
    .map_err(ApiError::Internal)?;
    let Some(project) = project else {
        return Ok(None); // not a Modrinth-known mod in our registry
    };
    let carried = state
        .modrinth
        .project_versions(&project, Some(&mc))
        .await
        .map_err(ApiError::Internal)?
        .iter()
        .any(|v| v.loaders.iter().any(|l| l.eq_ignore_ascii_case(&loader)));
    Ok(carried.then(|| {
        format!(
            "Modrinth already carries {modid} for Minecraft {mc} ({loader}) -- \
             add it via the Modrinth picker, not a self-hosted upload"
        )
    }))
}

#[derive(Deserialize)]
struct ForkReq {
    source: String,
    name: String,
}

/// Fork a pack into the caller's namespace: copy its config + static under
/// `u/<uid>/<name>` as a community draft with `fork_of` set to the source. The
/// caller may fork any published pack, or one they already own (their draft).
async fn fork_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Json(req): Json<ForkReq>,
) -> Result<(StatusCode, Json<PackConfig>), ApiError> {
    super::auth::not_suspended(&state, &identity).await?;
    super::auth::require_terms(&state, identity.uid).await?;
    let published = state
        .storage
        .load_pack_summary(&req.source)
        .await
        .map(|s| s.visibility == Visibility::Published)
        .unwrap_or(false);
    // a published pack is forkable by anyone; an unpublished one only by
    // somebody who can already see it
    if !published {
        super::auth::authorize(&state, &identity, &req.source, PackLevel::View).await?;
    }
    let target = format!("u/{}/{}", identity.uid, req.name);
    let cfg = state
        .storage
        .duplicate_pack(
            &req.source,
            &target,
            None,
            identity.uid,
            Some(req.source.clone()),
        )
        .await?;
    state.events.pack(&target, "created");
    Ok((StatusCode::CREATED, Json(cfg)))
}

/// Record that the caller has accepted the rules of use.
async fn accept_terms(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<StatusCode, ApiError> {
    let uid = identity.uid;
    let acc = state.accounts.clone();
    tokio::task::spawn_blocking(move || acc.accept_terms(uid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("accept task: {e}")))??;
    Ok(StatusCode::NO_CONTENT)
}

/// The caller's own uploads and their moderation status.
async fn my_uploads(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<Vec<UploadRow>>, ApiError> {
    let uid = identity.uid;
    let acc = state.accounts.clone();
    let rows = tokio::task::spawn_blocking(move || acc.list_user_uploads(uid))
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("uploads task: {e}")))??;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::classfile::fixtures::jar;

    #[test]
    fn extract_upload_facts_reads_modid_loader_mc() {
        // 1.12.2 forge: mcmod.info carries modid + mcversion
        let forge = jar(&[(
            "mcmod.info",
            br#"[{"modid":"thaumcraft","version":"6","mcversion":"1.12.2"}]"#,
        )]);
        assert_eq!(
            extract_upload_facts(&forge),
            Some(("thaumcraft".into(), "forge".into(), "1.12.2".into()))
        );

        // fabric: id + depends.minecraft
        let fabric = jar(&[(
            "fabric.mod.json",
            br#"{"id":"sodium","depends":{"minecraft":">=1.20.1"}}"#,
        )]);
        assert_eq!(
            extract_upload_facts(&fabric),
            Some(("sodium".into(), "fabric".into(), "1.20.1".into()))
        );

        // no readable metadata -> None (let a human decide)
        assert_eq!(extract_upload_facts(b"not a jar"), None);
    }
}
