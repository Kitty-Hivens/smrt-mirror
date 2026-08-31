//! Build-job endpoints: trigger a build, poll its status, and tail the live
//! log over Server-Sent Events. All admin-gated. The orchestration lives in
//! `crate::jobs`; these handlers are the thin HTTP surface.

use super::ApiError;
use crate::accounts::{Identity, PackLevel};
use crate::authoring::BootstrapArgs;
use crate::domain::{LoaderSpec, VersionChannel};
use crate::jobs::{BuildDeps, BuildRequest, DryRun, Status};
use crate::state::AppState;
use axum::Extension;
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::middleware::from_fn_with_state;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

pub fn router(state: AppState) -> Router {
    build_router(state.clone()).merge(bootstrap_router(state))
}

/// Build + job polling: any signed-in member may reach it, and every handler
/// then gates on the pack the request is about -- `build_pack` at `Edit`, the
/// two job reads at `View`.
///
/// A job id is not a capability and must never be treated as one. It is minted
/// as a zero-padded millisecond plus a process-wide counter (see
/// [`crate::jobs::JobRegistry::create`], where that shape is load-bearing for
/// snapshot pruning), so it is enumerable by anyone who has watched one of
/// their own builds. A build log names the pack, its mods and whatever the
/// pre-publish check refused -- draft packs included -- so reading one is a
/// read of that pack and is gated like any other.
fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/authoring/packs/{pack_id}/build", post(build_pack))
        .route("/v1/jobs/{job_id}", get(job_status))
        .route("/v1/jobs/{job_id}/events", get(job_events))
        .layer(DefaultBodyLimit::max(crate::http::MAX_JSON_BODY))
        .layer(from_fn_with_state(
            state.clone(),
            super::auth::require_session,
        ))
        .with_state(state)
}

/// Bootstrap-from-archive seeds an official pack from an instance archive -- operator
/// content authoring, admin only. One of the two routes that take a whole
/// archive in one body, hence the archive ceiling rather than the upload one.
fn bootstrap_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/v1/authoring/packs/{pack_id}/bootstrap",
            post(bootstrap_pack),
        )
        .layer(DefaultBodyLimit::max(crate::http::MAX_ARCHIVE_BODY))
        .layer(from_fn_with_state(state.clone(), super::auth::require_auth))
        .with_state(state)
}

#[derive(Serialize)]
struct JobRef {
    job_id: String,
    kind: &'static str,
    pack_id: String,
}

#[derive(Deserialize)]
struct BuildParams {
    /// `?dry_run=true` computes + stashes the manifest without publishing, so the
    /// panel can preview and diff before committing a real build.
    #[serde(default)]
    dry_run: bool,
    /// Optional canonical `pack_version` override; default is the next
    /// auto-numbered `<base>.<counter>`.
    pack_version: Option<String>,
    /// Release channel stored on the built manifest: `release` | `beta` |
    /// `alpha`. Defaults to `beta` -- publishing a release is an explicit act.
    channel: Option<String>,
    /// Publish even though the pre-publish check found something that means the
    /// pack cannot start (#108). The build says so in its log, the audit trail
    /// records who asked, and the manifest carries what it was published over.
    /// Same permission as building: it is the author's own pack, and the point
    /// of the flag is that the decision is recorded rather than prevented.
    #[serde(default)]
    override_checks: bool,
    /// Publish over a version that already exists (#146). Refused without it,
    /// because rewriting a published version changes what people already have
    /// under that name.
    #[serde(default)]
    overwrite_version: bool,
    /// Build this commit rather than the head of the pack's history (#122).
    /// Absent -> the newest commit, which is what "build this pack" means.
    from_commit: Option<String>,
}

#[derive(Deserialize, Default)]
struct BuildBody {
    /// Curator-authored release notes stored on the built manifest
    /// (Modrinth `version.changelog` analog). Body rather than query:
    /// multi-line CommonMark does not belong in a URL.
    changelog: Option<String>,
    /// The same notes per language tag (`{"en": "...", "ru": "..."}`). Blank
    /// entries are dropped, so a language the curator left empty does not ship
    /// as an empty release note.
    #[serde(default)]
    changelog_i18n: Option<BTreeMap<String, String>>,
}

async fn build_pack(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(pack_id): Path<String>,
    Query(p): Query<BuildParams>,
    body: Option<Json<BuildBody>>,
) -> Result<Json<JobRef>, ApiError> {
    super::auth::authorize(&state, &identity, &pack_id, PackLevel::Edit).await?;
    let from_commit = resolve_build_commit(&state, &pack_id, &p).await?;
    let pack_version = p.pack_version.filter(|v| !v.trim().is_empty());
    let channel = match p.channel.as_deref() {
        None => VersionChannel::Beta,
        Some(c) => VersionChannel::parse(c)
            .ok_or_else(|| ApiError::BadRequest("channel must be release, beta or alpha".into()))?,
    };
    let (changelog, changelog_i18n) = release_notes(body.map(|Json(b)| b).unwrap_or_default());
    let job = state.jobs.spawn_build(
        pack_id,
        BuildDeps {
            storage: state.storage.clone(),
            config: state.config.clone(),
            registry: state.registry.clone(),
            accounts: state.accounts.clone(),
            modrinth: state.modrinth.clone(),
            harvest: Some(state.harvest.clone()),
            events: state.events.clone(),
        },
        BuildRequest {
            dry_run: p.dry_run,
            pack_version,
            channel,
            changelog,
            changelog_i18n,
            override_checks: p.override_checks,
            overwrite_version: p.overwrite_version,
            from_commit,
            actor: Some(identity),
        },
    );
    Ok(Json(JobRef {
        job_id: job.id.clone(),
        kind: job.kind,
        pack_id: job.pack_id.clone(),
    }))
}

/// Settle a build's release notes: the per-language map, and the untagged text
/// a client that matches no language reads.
///
/// Blank entries are dropped rather than shipped, so a language the curator
/// left empty is absent instead of publishing an empty note. When no untagged
/// text was given, it is taken from the map -- English first, then whichever
/// language sorts first -- because a note in some language beats no note, and
/// every existing client reads only the untagged field.
fn release_notes(body: BuildBody) -> (Option<String>, Option<BTreeMap<String, String>>) {
    let i18n = body
        .changelog_i18n
        .map(|m| {
            m.into_iter()
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                .collect::<BTreeMap<_, _>>()
        })
        .filter(|m| !m.is_empty());
    let changelog = body
        .changelog
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            i18n.as_ref()
                .and_then(|m| m.get("en").or_else(|| m.values().next()))
                .cloned()
        });
    (changelog, i18n)
}

/// Which stored state this build should turn into a manifest (#122).
///
/// A real build is made from a commit, never from the live config: with edits
/// merging live, the config on disk is whatever is on screen at that instant,
/// and pressing build while somebody is mid-rename would ship half a word. A
/// dry run is the opposite question -- "what would what I have right now
/// build?" -- so it reads the live config unless a commit was named.
///
/// Refusing a publish that has uncommitted work is the point rather than a
/// safety rail: the alternative is a build that silently ships something older
/// than what the person pressing it is looking at.
async fn resolve_build_commit(
    state: &AppState,
    pack_id: &str,
    p: &BuildParams,
) -> Result<Option<String>, ApiError> {
    if let Some(id) = p
        .from_commit
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // named explicitly: rebuilding an older state is a legitimate thing to
        // ask for, and it is unambiguous, so nothing else needs checking
        state.storage.load_commit(pack_id, id).await?;
        return Ok(Some(id.to_string()));
    }
    if p.dry_run {
        return Ok(None);
    }
    let Some(head) = state.storage.commit_head(pack_id).await else {
        return Err(ApiError::Conflict(
            "this pack has no commits yet, and a build is made from one -- declare a checkpoint first".into(),
        ));
    };
    let live = state.storage.load_pack_config(pack_id).await?;
    let head_config = state.storage.load_commit_config(pack_id, &head).await.ok();
    let pending = crate::authoring::uncommitted(head_config.as_ref(), &live);
    if pending > 0 {
        return Err(ApiError::Conflict(format!(
            "{pending} change(s) since the last commit; commit them or build that commit by name"
        )));
    }
    Ok(Some(head))
}

#[derive(Deserialize)]
struct BootstrapParams {
    display_name: Option<String>,
    tagline: Option<String>,
    minecraft_version: String,
    loader_name: Option<String>,
    loader_version: String,
    java_major: Option<u32>,
}

async fn bootstrap_pack(
    State(state): State<AppState>,
    Path(pack_id): Path<String>,
    Query(p): Query<BootstrapParams>,
    body: Bytes,
) -> Json<JobRef> {
    let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
    let args = BootstrapArgs {
        pack_id: pack_id.clone(),
        display_name: nonempty(p.display_name).unwrap_or_else(|| pack_id.clone()),
        tagline: p.tagline.unwrap_or_default(),
        minecraft_version: p.minecraft_version,
        loader: LoaderSpec {
            name: nonempty(p.loader_name).unwrap_or_else(|| "forge".into()),
            version: p.loader_version,
        },
        java_major: p.java_major.unwrap_or(8),
        storage: state.storage.root().to_path_buf(),
    };
    let job = state
        .jobs
        .spawn_bootstrap(pack_id, args, body.to_vec(), state.storage.clone());
    Json(JobRef {
        job_id: job.id.clone(),
        kind: job.kind,
        pack_id: job.pack_id.clone(),
    })
}

#[derive(Serialize)]
struct JobStatusResp {
    job_id: String,
    kind: String,
    pack_id: String,
    status: Status,
    log: Vec<String>,
    /// What the pre-publish check would stop this build on (#108). Present on a
    /// dry run too, where it is a warning rather than a refusal. The panel reads
    /// it to tell "the gate said no" -- which an override answers -- from a
    /// failure no override can help.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocked: Vec<String>,
    /// Present only for a finished dry-run (preview) build.
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<DryRun>,
}

/// A job's log and outcome, for whoever may read the pack it is about.
async fn job_status(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(job_id): Path<String>,
) -> Result<Json<JobStatusResp>, ApiError> {
    if let Some(job) = state.jobs.get(&job_id) {
        super::auth::authorize(&state, &identity, &job.pack_id, PackLevel::View).await?;
        let (log, status) = job.since(0);
        return Ok(Json(JobStatusResp {
            job_id: job.id.clone(),
            kind: job.kind.to_string(),
            pack_id: job.pack_id.clone(),
            status,
            log,
            blocked: job.blocked(),
            result: job.result(),
        }));
    }
    // Not in memory (evicted, or from before a restart): answer from the
    // persisted snapshot instead of a 404, so a job id stays meaningful for
    // the lifetime of its snapshot. Dry-run results are memory-only.
    let snap = state
        .storage
        .load_job_snapshot(&job_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    super::auth::authorize(&state, &identity, &snap.pack_id, PackLevel::View).await?;
    Ok(Json(JobStatusResp {
        job_id: snap.job_id,
        kind: snap.kind,
        pack_id: snap.pack_id,
        status: snap.status,
        log: snap.log,
        // memory-only, like the dry-run result: a job answered from its
        // snapshot is one nobody is going to override any more
        blocked: Vec::new(),
        result: None,
    }))
}

/// The same log as it is written, for whoever may read the pack it is about.
async fn job_events(
    State(state): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(job_id): Path<String>,
) -> Result<Response, ApiError> {
    let job = state.jobs.get(&job_id).ok_or(ApiError::NotFound)?;
    super::auth::authorize(&state, &identity, &job.pack_id, PackLevel::View).await?;
    let stream = async_stream::stream! {
        let mut sent = 0usize;
        loop {
            let (lines, status) = job.since(sent);
            for line in lines {
                sent += 1;
                yield Ok::<Event, Infallible>(Event::default().event("line").data(line));
            }
            match status {
                Status::Running => {
                    // Notify wakes us immediately on a new line; the timeout
                    // bounds latency if a wake is ever missed (we re-read from
                    // `sent`, so nothing is lost either way).
                    let _ = tokio::time::timeout(Duration::from_millis(500), job.wait()).await;
                }
                Status::Done => {
                    yield Ok(Event::default().event("done").data("ok"));
                    break;
                }
                Status::Failed => {
                    yield Ok(Event::default().event("failed").data("failed"));
                    break;
                }
            }
        }
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(changelog: Option<&str>, i18n: &[(&str, &str)]) -> BuildBody {
        BuildBody {
            changelog: changelog.map(str::to_string),
            changelog_i18n: (!i18n.is_empty()).then(|| {
                i18n.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            }),
        }
    }

    #[test]
    fn a_language_left_empty_does_not_ship_as_an_empty_note() {
        let (untagged, map) =
            release_notes(body(None, &[("en", "Fixed the crash."), ("ru", "  ")]));
        let map = map.expect("english survives");
        assert_eq!(map.keys().collect::<Vec<_>>(), ["en"]);
        // and the untagged copy every existing client reads is filled from it
        assert_eq!(untagged.as_deref(), Some("Fixed the crash."));
    }

    #[test]
    fn nothing_written_stays_nothing() {
        assert_eq!(release_notes(body(None, &[])), (None, None));
        assert_eq!(
            release_notes(body(Some("   "), &[("ru", "")])),
            (None, None)
        );
    }

    #[test]
    fn the_untagged_note_falls_back_to_english_then_to_whatever_exists() {
        let (untagged, _) = release_notes(body(None, &[("ru", "Починили."), ("en", "Fixed.")]));
        assert_eq!(untagged.as_deref(), Some("Fixed."));

        // no English written: a note in some language beats no note at all
        let (untagged, _) = release_notes(body(None, &[("ru", "Починили.")]));
        assert_eq!(untagged.as_deref(), Some("Починили."));
    }

    #[test]
    fn an_explicit_untagged_note_is_never_overwritten() {
        let (untagged, map) = release_notes(body(Some("Read me"), &[("en", "Fixed.")]));
        assert_eq!(untagged.as_deref(), Some("Read me"));
        assert_eq!(map.unwrap().get("en").map(String::as_str), Some("Fixed."));
    }

    #[test]
    fn language_tags_are_normalised_so_one_language_is_one_entry() {
        let (_, map) = release_notes(body(None, &[(" EN ", "Fixed."), ("ru", "Починили.")]));
        let map = map.unwrap();
        assert_eq!(map.keys().collect::<Vec<_>>(), ["en", "ru"]);
    }
}
