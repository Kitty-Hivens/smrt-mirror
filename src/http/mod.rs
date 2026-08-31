//! HTTP layer (controllers): the public `/v1` read API, the gated `/v1` write +
//! authoring API (`/v1/registry`, `/v1/authoring`, `/v1/users`, ...), and the
//! shared response error. `router` assembles the full application router from
//! the halves.

pub mod admin;
pub mod apidoc;
pub mod auth;
pub mod error;
pub mod etag;
pub mod jobs;
pub mod member;
pub mod page;
pub mod panel;
pub mod public;
pub mod registry;

pub use error::ApiError;

use crate::accounts::Identity;
use crate::state::AppState;
use axum::Router;
use tower_http::compression::{CompressionLayer, DefaultPredicate, Predicate};

/// Ceiling on a request body that carries one file: a cache jar, a pack static
/// asset, a member's upload. Comfortably above any real mod jar (the largest
/// on a mirror are tens of megabytes), and the same number as the per-entry cap
/// the archive reader applies to a single file unpacked out of a zip.
///
/// Every one of these ceilings is a memory ceiling, not just a size gate: the
/// handlers extract `Bytes`, so axum buffers the whole body in RAM before the
/// handler runs. A concurrent burst holds that much per request, which is why
/// the routes are sized by what they actually carry rather than all sharing the
/// largest number any of them needs.
pub(crate) const MAX_UPLOAD_BODY: usize = 512 * 1024 * 1024;

/// Ceiling for the two routes that take a whole Minecraft instance archive in
/// one body -- `bootstrap` and `validate`. Nothing else needs gigabytes, and
/// nginx in front is raised to match this one (see the deploy config), since
/// the smaller of the two wins.
///
/// The bootstrap path copies the buffered body once more before unpacking it,
/// so a request near this limit holds twice this much for its lifetime. That is
/// the cost of accepting an archive in one shot; it is bounded here and stated
/// in `docs/operations.md` rather than discovered under load.
pub(crate) const MAX_ARCHIVE_BODY: usize = 8 * 1024 * 1024 * 1024;

/// Ceiling for a body that is only ever JSON or a CRDT update -- build
/// requests, document sync. Generous for both; it exists so a route that can
/// never carry a file does not inherit a file-sized ceiling.
pub(crate) const MAX_JSON_BODY: usize = 8 * 1024 * 1024;

/// Best-effort audit write shared by the admin and registry write paths: record
/// who did what. A failure is logged, never raised -- the audited action already
/// happened, so a lost trail entry must not turn a successful operation into an
/// error for the caller.
pub(crate) async fn audit(
    state: &AppState,
    who: &Identity,
    action: &str,
    target: Option<&str>,
    detail: Option<&str>,
) {
    let acc = state.accounts.clone();
    let (uid, login, action) = (who.uid, who.login.clone(), action.to_string());
    let (target, detail) = (target.map(String::from), detail.map(String::from));
    let res = tokio::task::spawn_blocking(move || {
        acc.record_audit(uid, &login, &action, target.as_deref(), detail.as_deref())
    })
    .await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "audit write failed"),
        Err(e) => tracing::warn!(error = %e, "audit task failed"),
    }
}

/// Response compression for everything the mirror answers in text.
///
/// It lives here rather than in the reverse proxy because the mirror is the
/// product and the proxy is one deployment's choice: a self-hoster behind
/// Caddy, behind nothing, or behind a proxy nobody configured would otherwise
/// ship manifests and registry listings as plain text. A manifest is tens of
/// kilobytes of repetitive JSON, which is the shape that compresses best.
///
/// What is left alone matters as much as what is not. The default predicate
/// already skips tiny bodies, images and event streams -- compressing an SSE
/// body would hold each line in an encoder buffer instead of delivering it,
/// which is the one thing a live log must not do. Added to that: jars and the
/// archives, because a zip re-compresses to roughly its own size and the CPU
/// spent proving it is the whole cost.
fn compression() -> tower_http::compression::CompressionLayer<impl Predicate> {
    use tower_http::compression::predicate::NotForContentType;
    CompressionLayer::new().compress_when(
        DefaultPredicate::new()
            .and(NotForContentType::const_new("application/java-archive"))
            .and(NotForContentType::const_new("application/zip"))
            .and(NotForContentType::const_new("application/octet-stream"))
            .and(NotAPart),
    )
}

/// A range names bytes of the representation the client receives, so encoding
/// one after slicing it would describe the answer with the wrong ruler -- the
/// client would splice compressed bytes into a file at offsets counted in
/// plain ones.
#[derive(Clone, Copy)]
struct NotAPart;

impl Predicate for NotAPart {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: axum::body::HttpBody,
    {
        response.status() != axum::http::StatusCode::PARTIAL_CONTENT
            && !response
                .headers()
                .contains_key(axum::http::header::CONTENT_RANGE)
    }
}

/// The full application router: public reads, admin writes + authoring, build
/// jobs, the panel auth endpoints, and the embedded panel under `/admin`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(public::router(state.clone()))
        .merge(admin::router(state.clone()))
        .merge(member::router(state.clone()))
        .merge(registry::router(state.clone()))
        .merge(auth::router(state.clone()))
        .merge(jobs::router(state.clone()))
        .merge(panel::router())
        .merge(apidoc::router())
        // inside compression, so a tag names the answer rather than one
        // encoding of it, and a 304 leaves nothing to encode
        .layer(axum::middleware::from_fn(etag::tag_json))
        .layer(compression())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::PackLevel;
    use crate::config::Config;
    use std::str::FromStr;

    // Assembling the full router merges every sub-router into one matchit tree;
    // an overlapping route would panic here, which is exactly the startup crash
    // we want a test to catch rather than a deploy.
    // Community pack ids carry slashes (u/<uid>/<pack>); they ride in a
    // single `:pack_id` segment percent-encoded, so this pins that axum decodes
    // %2F back into the slashed id the handler sees. If this ever regresses, the
    // whole community-authoring URL scheme breaks.
    #[tokio::test]
    async fn path_param_decodes_percent_encoded_slashes() {
        use axum::body::Body;
        use axum::extract::Path;
        use axum::http::Request;
        use axum::routing::get;
        use tower::ServiceExt;

        async fn echo(Path(id): Path<String>) -> String {
            id
        }
        let app = Router::new().route("/p/{id}", get(echo));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/p/u%2F42%2FMyPack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"u/42/MyPack");
    }

    // ── the gates, as HTTP sees them (ADR 0006) ─────────────────────────────
    //
    // The levels are compared in one place, which is exactly why one place
    // getting it wrong would be silent everywhere. These drive the real router
    // with a real session, so a handler that forgets to gate, or gates at the
    // wrong level, fails here rather than in somebody's pack.

    fn test_state(dir: &std::path::Path) -> AppState {
        let config = Config {
            bind_addr: std::net::SocketAddr::from_str("127.0.0.1:0").unwrap(),
            storage_dir: dir.to_path_buf(),
            admin_token: None,
            cookie_secure: false,
            mirror_base: "http://localhost".into(),
            github_client_id: None,
            github_client_secret: None,
            admin_github_uids: Vec::new(),
            debug_token: None,
            debug_github_uids: Vec::new(),
        };
        AppState::new(config).unwrap()
    }

    fn sample_pack(
        pack_id: &str,
        visibility: crate::domain::Visibility,
    ) -> crate::domain::PackConfig {
        crate::domain::PackConfig {
            pack_id: pack_id.into(),
            display_name: pack_id.into(),
            tagline: String::new(),
            minecraft_version: "1.21.1".into(),
            loader: crate::domain::LoaderSpec {
                name: "neoforge".into(),
                version: "21.1.248".into(),
            },
            java_major: 21,
            version: Some("0.1".into()),
            tags: Vec::new(),
            featured: false,
            mods: Vec::new(),
            assets: Vec::new(),
            auth: None,
            pack_meta: crate::domain::PackMeta::default(),
            owner: 1,
            tier: crate::domain::PackTier::Official,
            visibility,
            fork_of: None,
        }
    }

    /// The same request as [`call`], with what came back -- for the handlers
    /// whose answer is the point rather than their status.
    async fn read(
        app: &axum::Router,
        uri: &str,
        session: Option<&str>,
    ) -> (axum::http::StatusCode, String) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let mut req = Request::builder().method("GET").uri(uri);
        if let Some(sid) = session {
            req = req.header(axum::http::header::COOKIE, format!("smrt_session={sid}"));
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A write, with what came back -- for the refusals that are worth reading.
    async fn write(
        app: &axum::Router,
        method: &str,
        uri: &str,
        session: Option<&str>,
        body: Option<&str>,
    ) -> (axum::http::StatusCode, String) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(sid) = session {
            req = req.header(axum::http::header::COOKIE, format!("smrt_session={sid}"));
        }
        let req = match body {
            Some(json) => req
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        session: Option<&str>,
        body: Option<&str>,
    ) -> axum::http::StatusCode {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(sid) = session {
            req = req.header(axum::http::header::COOKIE, format!("smrt_session={sid}"));
        }
        let req = match body {
            Some(json) => req
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        app.clone().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_level_admits_exactly_what_it_names() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        state
            .storage
            .save_pack_config(
                "Create",
                &sample_pack("Create", crate::domain::Visibility::Draft),
            )
            .await
            .unwrap();
        let sid = state.accounts.sign_in_github(42, "helper", None).unwrap();
        let app = router(state.clone());
        let cfg = serde_json::to_string(&sample_pack("Create", crate::domain::Visibility::Draft))
            .unwrap();

        // nothing granted: a member cannot even read somebody else's pack
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/authoring/packs/Create/config",
                Some(&sid),
                None
            )
            .await,
            StatusCode::FORBIDDEN
        );

        // view reads and does not write
        state
            .accounts
            .grant_pack_access("Create", 42, PackLevel::View, 1)
            .unwrap();
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/authoring/packs/Create/config",
                Some(&sid),
                None
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/packs/Create/config",
                Some(&sid),
                Some(&cfg)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/commits",
                Some(&sid),
                Some(r#"{"message":"x"}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );

        // edit writes but does not hand out access or delete
        state
            .accounts
            .grant_pack_access("Create", 42, PackLevel::Edit, 1)
            .unwrap();
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/packs/Create/config",
                Some(&sid),
                Some(&cfg)
            )
            .await,
            StatusCode::CREATED
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/access",
                Some(&sid),
                Some(r#"{"github_uid":77,"level":"view"}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/v1/authoring/packs/Create",
                Some(&sid),
                None
            )
            .await,
            StatusCode::FORBIDDEN
        );

        // own does both
        state
            .accounts
            .grant_pack_access("Create", 42, PackLevel::Own, 1)
            .unwrap();
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/access",
                Some(&sid),
                Some(r#"{"github_uid":77,"level":"view"}"#)
            )
            .await,
            StatusCode::NO_CONTENT
        );

        // and the panel is told the same answer the gate just gave, rather than
        // guessing it from the pack id: a granted keeper is a keeper.
        assert_eq!(
            read(&app, "/v1/authoring/packs/Create/access/mine", Some(&sid)).await,
            (StatusCode::OK, r#"{"level":"own"}"#.to_string())
        );
        let stranger = state.accounts.sign_in_github(99, "stranger", None).unwrap();
        assert_eq!(
            read(
                &app,
                "/v1/authoring/packs/Create/access/mine",
                Some(&stranger)
            )
            .await,
            (StatusCode::OK, "{}".to_string()),
            "no level is an answer, not an error"
        );
    }

    // Being answered is the half of a discussion that makes people come back to
    // it. What matters here is who is told and who is not: the person who acted
    // is never told about their own act, and the pack's keepers hear about a
    // report without having to open the tab to find it.
    #[tokio::test]
    async fn an_answer_reaches_the_people_it_is_for_and_nobody_else() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let cfg = sample_pack("Create", crate::domain::Visibility::Published);
        state
            .storage
            .save_pack_config("Create", &cfg)
            .await
            .unwrap();
        state
            .storage
            .save_pack_summary(&crate::authoring::make_pack_summary(
                &cfg,
                "0.1.0",
                "http://localhost",
            ))
            .await
            .unwrap();
        // an official pack, so its keepers are the mirror's operators
        let keeper = state
            .accounts
            .sign_in_github(1, "keeper", Some(crate::accounts::Role::Admin))
            .unwrap();
        let reporter = state.accounts.sign_in_github(42, "reporter", None).unwrap();
        let other = state.accounts.sign_in_github(43, "other", None).unwrap();
        let app = router(state.clone());

        call(
            &app,
            "POST",
            "/v1/authoring/packs/Create/issues",
            Some(&reporter),
            Some(r#"{"title":"a mod crashes"}"#),
        )
        .await;
        assert_eq!(
            state.accounts.unread_count(1).unwrap(),
            1,
            "whoever keeps the pack hears that a report was opened"
        );
        assert_eq!(
            state.accounts.unread_count(42).unwrap(),
            0,
            "and the reporter is not told about their own report"
        );

        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/comments",
                Some(&other),
                Some(r#"{"body":"same here"}"#)
            )
            .await,
            StatusCode::CREATED
        );
        assert_eq!(
            state.accounts.unread_count(42).unwrap(),
            1,
            "the reporter is told they were answered"
        );
        assert_eq!(state.accounts.unread_count(43).unwrap(), 0);

        // the keeper closes it: the people in the discussion hear the decision
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/close",
                Some(&keeper),
                None
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(state.accounts.unread_count(42).unwrap(), 2);
        assert_eq!(
            state.accounts.unread_count(43).unwrap(),
            1,
            "including whoever spoke in it"
        );

        // a list that only grows is read a page at a time, newest first
        let (_, first) = read(&app, "/v1/me/notifications?limit=1", Some(&reporter)).await;
        assert_eq!(first.matches("\"id\":").count(), 1, "one row: {first}");
        assert!(
            first.contains("\"unread\":2"),
            "the count is the whole list: {first}"
        );

        // reading is the caller's own list, and marking read is scoped to it
        let (status, body) = read(&app, "/v1/me/notifications?unread=true", Some(&reporter)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"unread\":2"), "{body}");
        assert!(
            body.contains("a mod crashes"),
            "the thread's own words: {body}"
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/me/notifications/read",
                Some(&reporter),
                Some("{}")
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(state.accounts.unread_count(42).unwrap(), 0);
        assert_eq!(
            state.accounts.unread_count(43).unwrap(),
            1,
            "somebody else's list is not the caller's to mark"
        );
    }

    // Two stops that answer different questions: a pack's keepers decide who
    // writes in their discussion, the mirror's operators decide who puts
    // anything on it at all. Neither touches reading.
    // A feed reader has no session, so the address is the credential. What
    // matters is that it names exactly one account's own list, that an unknown
    // one says nothing, and that rotating retires the old one.
    #[tokio::test]
    async fn a_feed_address_is_one_accounts_own_and_can_be_retired() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let cfg = sample_pack("Create", crate::domain::Visibility::Published);
        state
            .storage
            .save_pack_config("Create", &cfg)
            .await
            .unwrap();
        state
            .storage
            .save_pack_summary(&crate::authoring::make_pack_summary(
                &cfg,
                "0.1.0",
                "http://localhost",
            ))
            .await
            .unwrap();
        let reporter = state.accounts.sign_in_github(42, "reporter", None).unwrap();
        let other = state.accounts.sign_in_github(43, "other", None).unwrap();
        let app = router(state.clone());

        call(
            &app,
            "POST",
            "/v1/authoring/packs/Create/issues",
            Some(&reporter),
            Some(r#"{"title":"a mod crashes"}"#),
        )
        .await;
        call(
            &app,
            "POST",
            "/v1/authoring/threads/1/comments",
            Some(&other),
            Some(r#"{"body":"same here"}"#),
        )
        .await;

        let (_, minted) = read(&app, "/v1/me/feed-key", Some(&reporter)).await;
        let key = minted
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches("\"}")
            .to_string();
        let (status, feed) = read(&app, &format!("/v1/feed.atom?key={key}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(feed.contains("<feed"), "{feed}");
        assert!(feed.contains("a mod crashes"), "their own list: {feed}");

        // nobody else's, and nothing at all for a key that names no account
        assert_eq!(
            read(&app, "/v1/feed.atom?key=not-a-key", None).await.0,
            StatusCode::NOT_FOUND
        );

        // rotating retires the address that was handed out
        let (_, rotated) = write(&app, "POST", "/v1/me/feed-key", Some(&reporter), None).await;
        assert!(!rotated.contains(&key), "a new address: {rotated}");
        assert_eq!(
            read(&app, &format!("/v1/feed.atom?key={key}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_suspended_account_writes_nowhere_and_still_reads_everywhere() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let cfg = sample_pack("Create", crate::domain::Visibility::Published);
        state
            .storage
            .save_pack_config("Create", &cfg)
            .await
            .unwrap();
        state
            .storage
            .save_pack_summary(&crate::authoring::make_pack_summary(
                &cfg,
                "0.1.0",
                "http://localhost",
            ))
            .await
            .unwrap();
        let mine = sample_pack("u/42/Mine", crate::domain::Visibility::Draft);
        state
            .storage
            .save_pack_config("u/42/Mine", &mine)
            .await
            .unwrap();
        let theirs = state.accounts.sign_in_github(42, "theirs", None).unwrap();
        let operator = state
            .accounts
            .sign_in_github(1, "operator", Some(crate::accounts::Role::Admin))
            .unwrap();
        state.accounts.accept_terms(42).unwrap();
        let app = router(state.clone());
        let body = serde_json::to_string(&mine).unwrap();

        // before: they author their own pack and speak on a published one
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/packs/u%2F42%2FMine/config",
                Some(&theirs),
                Some(&body)
            )
            .await,
            StatusCode::CREATED
        );

        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/users/42/suspension",
                Some(&operator),
                r#"{"reason":"a pack made to insult people"}"#.into()
            )
            .await,
            StatusCode::NO_CONTENT
        );

        // after: nothing they write lands, on their own pack or anybody's
        let (status, refusal) = write(
            &app,
            "PUT",
            "/v1/authoring/packs/u%2F42%2FMine/config",
            Some(&theirs),
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            refusal.contains("insult"),
            "the reason reaches them: {refusal}"
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/issues",
                Some(&theirs),
                Some(r#"{"title":"anything"}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/me/forks",
                Some(&theirs),
                Some(r#"{"source":"Create","name":"copy"}"#)
            )
            .await,
            StatusCode::FORBIDDEN,
            "including making something new to say it in"
        );

        // and everything they could read, they still read
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/authoring/packs/u%2F42%2FMine/config",
                Some(&theirs),
                None
            )
            .await,
            StatusCode::OK
        );
        let (_, me) = read(&app, "/v1/me", Some(&theirs)).await;
        assert!(me.contains("insult"), "their own page says why: {me}");

        // an operator is not somebody another operator can silence
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/users/1/suspension",
                Some(&operator),
                Some("{}")
            )
            .await,
            StatusCode::BAD_REQUEST
        );

        // lifted, and they are back
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/v1/users/42/suspension",
                Some(&operator),
                None
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/packs/u%2F42%2FMine/config",
                Some(&theirs),
                Some(&body)
            )
            .await,
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn a_block_stops_the_next_message_not_the_record() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let cfg = sample_pack("Create", crate::domain::Visibility::Published);
        state
            .storage
            .save_pack_config("Create", &cfg)
            .await
            .unwrap();
        state
            .storage
            .save_pack_summary(&crate::authoring::make_pack_summary(
                &cfg,
                "0.1.0",
                "http://localhost",
            ))
            .await
            .unwrap();
        let loud = state.accounts.sign_in_github(42, "loud", None).unwrap();
        let keeper = state.accounts.sign_in_github(43, "keeper", None).unwrap();
        state
            .accounts
            .grant_pack_access("Create", 43, PackLevel::Edit, 1)
            .unwrap();
        let app = router(state.clone());

        call(
            &app,
            "POST",
            "/v1/authoring/packs/Create/issues",
            Some(&loud),
            Some(r#"{"title":"first"}"#),
        )
        .await;

        // moderation, not ownership: the same rung that hides a comment
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/blocks",
                Some(&loud),
                Some(r#"{"github_uid":43}"#)
            )
            .await,
            StatusCode::FORBIDDEN,
            "a speaker does not get to block the pack's keepers"
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/blocks",
                Some(&keeper),
                Some(r#"{"github_uid":42,"reason":"flooding"}"#)
            )
            .await,
            StatusCode::NO_CONTENT
        );

        // what was blocked is the writing, and the refusal says why rather than
        // shutting a door with no word through it
        let (status, body) = write(
            &app,
            "POST",
            "/v1/authoring/threads/1/comments",
            Some(&loud),
            Some(r#"{"body":"again"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("flooding"), "the reason reaches them: {body}");
        // and the panel can learn it before offering a box that cannot work
        let (_, mine) = read(&app, "/v1/authoring/packs/Create/access/mine", Some(&loud)).await;
        assert!(mine.contains("flooding"), "{mine}");
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/issues",
                Some(&loud),
                Some(r#"{"title":"second"}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        // and what stays is the record: a block is not a way to unsay something
        assert_eq!(read(&app, "/v1/threads/1", None).await.0, StatusCode::OK);

        // the keepers cannot be blocked out of their own pack
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/blocks",
                Some(&keeper),
                Some(r#"{"github_uid":43}"#)
            )
            .await,
            StatusCode::BAD_REQUEST
        );

        // lifting it puts the person back where they were
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/v1/authoring/packs/Create/blocks/42",
                Some(&keeper),
                None
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/comments",
                Some(&loud),
                Some(r#"{"body":"again"}"#)
            )
            .await,
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn a_discussion_is_as_public_as_its_pack() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        for (id, vis) in [
            ("Create", crate::domain::Visibility::Published),
            ("Secret", crate::domain::Visibility::Draft),
        ] {
            let cfg = sample_pack(id, vis);
            state.storage.save_pack_config(id, &cfg).await.unwrap();
            state
                .storage
                .save_pack_summary(&crate::authoring::make_pack_summary(
                    &cfg,
                    "0.1.0",
                    "http://localhost",
                ))
                .await
                .unwrap();
        }
        let sid = state.accounts.sign_in_github(42, "helper", None).unwrap();
        let app = router(state.clone());

        // anyone signed in may report on a published pack they were never let into
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/issues",
                Some(&sid),
                Some(r#"{"title":"a mod crashes"}"#)
            )
            .await,
            StatusCode::CREATED
        );
        // and not on one they cannot see
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Secret/issues",
                Some(&sid),
                Some(r#"{"title":"nosy"}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );

        // the published pack's discussion reads with no session at all; the
        // draft's is as invisible as the draft
        assert_eq!(
            call(&app, "GET", "/v1/packs/Create/threads", None, None).await,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "GET", "/v1/threads/1", None, None).await,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "GET", "/v1/packs/Secret/threads", None, None).await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn moderating_and_deciding_belong_to_the_pack_not_the_speaker() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let cfg = sample_pack("Create", crate::domain::Visibility::Published);
        state
            .storage
            .save_pack_config("Create", &cfg)
            .await
            .unwrap();
        state
            .storage
            .save_pack_summary(&crate::authoring::make_pack_summary(
                &cfg,
                "0.1.0",
                "http://localhost",
            ))
            .await
            .unwrap();
        let sid = state.accounts.sign_in_github(42, "helper", None).unwrap();
        let other = state.accounts.sign_in_github(43, "stranger", None).unwrap();
        let app = router(state.clone());

        call(
            &app,
            "POST",
            "/v1/authoring/packs/Create/issues",
            Some(&sid),
            Some(r#"{"title":"a mod crashes"}"#),
        )
        .await;
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/comments",
                Some(&other),
                Some(r#"{"body":"mine too"}"#)
            )
            .await,
            StatusCode::CREATED,
            "anyone who can read a discussion can join it"
        );

        // a speaker cannot moderate the discussion they are in
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/comments/1/hidden",
                Some(&other),
                Some(r#"{"hidden":true}"#)
            )
            .await,
            StatusCode::FORBIDDEN
        );
        // but whoever keeps the pack can
        state
            .accounts
            .grant_pack_access("Create", 43, PackLevel::Edit, 1)
            .unwrap();
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/authoring/comments/1/hidden",
                Some(&other),
                Some(r#"{"hidden":true}"#)
            )
            .await,
            StatusCode::NO_CONTENT
        );

        // the reporter may close their own thread; a settled one does not settle twice
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/close",
                Some(&sid),
                None
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/threads/1/close",
                Some(&sid),
                None
            )
            .await,
            StatusCode::CONFLICT
        );
    }

    // A build log names the pack it is about, the mods in it, and whatever the
    // pre-publish check refused -- on a draft nobody else may see. The job id is
    // a millisecond and a counter, so anyone who has watched one of their own
    // builds can enumerate other people's; the gate has to be the pack, not the
    // id.
    #[tokio::test]
    async fn a_build_log_is_only_as_readable_as_the_pack_it_is_about() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        state
            .storage
            .save_pack_config(
                "Create",
                &sample_pack("Create", crate::domain::Visibility::Draft),
            )
            .await
            .unwrap();
        let operator = state
            .accounts
            .sign_in_github(1, "operator", Some(crate::accounts::Role::Admin))
            .unwrap();
        let stranger = state.accounts.sign_in_github(42, "stranger", None).unwrap();
        let app = router(state.clone());

        let (status, started) = write(
            &app,
            "POST",
            "/v1/authoring/packs/Create/build?dry_run=true",
            Some(&operator),
            Some("{}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{started}");
        let job_id = serde_json::from_str::<serde_json::Value>(&started)
            .unwrap()
            .get("job_id")
            .and_then(|v| v.as_str())
            .expect("a job id")
            .to_string();

        for path in [
            format!("/v1/jobs/{job_id}"),
            format!("/v1/jobs/{job_id}/events"),
        ] {
            assert_eq!(
                read(&app, &path, Some(&stranger)).await.0,
                StatusCode::FORBIDDEN,
                "a member who cannot read the pack cannot read its build: {path}"
            );
        }
        assert_eq!(
            read(&app, &format!("/v1/jobs/{job_id}"), Some(&operator))
                .await
                .0,
            StatusCode::OK,
            "whoever keeps the pack still reads it"
        );

        // and being let into the pack is what lets them read it -- the same
        // answer the rest of the authoring surface gives
        state
            .accounts
            .grant_pack_access("Create", 42, PackLevel::View, 1)
            .unwrap();
        assert_eq!(
            read(&app, &format!("/v1/jobs/{job_id}"), Some(&stranger))
                .await
                .0,
            StatusCode::OK
        );
    }

    // Two reviewers pressing merge at the same moment. The decision is one
    // decision: the pack takes one merge commit, and the reviewer who lost is
    // refused before writing anything rather than after writing a second commit
    // of the same proposal.
    #[tokio::test]
    async fn one_proposal_merges_once_however_many_press_it() {
        use axum::http::StatusCode;
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        for id in ["Create", "u/42/Fork"] {
            let cfg = sample_pack(id, crate::domain::Visibility::Published);
            state.storage.save_pack_config(id, &cfg).await.unwrap();
            state
                .storage
                .save_pack_summary(&crate::authoring::make_pack_summary(
                    &cfg,
                    "0.1.0",
                    "http://localhost",
                ))
                .await
                .unwrap();
        }
        let proposer = state.accounts.sign_in_github(42, "proposer", None).unwrap();
        let one = state
            .accounts
            .sign_in_github(1, "keeper-one", Some(crate::accounts::Role::Admin))
            .unwrap();
        let two = state
            .accounts
            .sign_in_github(2, "keeper-two", Some(crate::accounts::Role::Admin))
            .unwrap();
        let app = router(state.clone());

        // the fork has something to offer
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/u%2F42%2FFork/commits",
                Some(&proposer),
                Some(r#"{"message":"a change worth offering"}"#)
            )
            .await,
            StatusCode::CREATED
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/v1/authoring/packs/Create/proposals",
                Some(&proposer),
                Some(r#"{"source_pack":"u/42/Fork","title":"take mine"}"#)
            )
            .await,
            StatusCode::CREATED
        );

        let before = state.storage.commit_log("Create", None, 100).await.unwrap();
        let merge = |sid: String| {
            let app = app.clone();
            async move {
                write(
                    &app,
                    "POST",
                    "/v1/authoring/threads/1/merge",
                    Some(&sid),
                    Some("{}"),
                )
                .await
            }
        };
        let (a, b) = tokio::join!(merge(one), merge(two));

        let mut codes = [a.0, b.0];
        codes.sort_by_key(|c| c.as_u16());
        assert_eq!(
            codes,
            [StatusCode::OK, StatusCode::CONFLICT],
            "one takes it, the other is told it was taken: {a:?} {b:?}"
        );
        let after = state.storage.commit_log("Create", None, 100).await.unwrap();
        assert_eq!(
            after.len(),
            before.len() + 1,
            "one decision leaves one commit behind, not two"
        );
    }

    // A wildcard segment is `{*name}`; `{{` and `}}` are how axum 0.8 escapes a
    // literal brace. Written `{{*name}}` a route quietly stops matching anything
    // real and every request for it falls through to the SPA -- which is a 200
    // page of HTML where an asset or a file was asked for, not an error anyone
    // notices. Both wildcard routes are pinned here by behaviour: reaching the
    // handler at all is the assertion.
    #[tokio::test]
    async fn wildcard_routes_reach_their_handlers() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            bind_addr: std::net::SocketAddr::from_str("127.0.0.1:0").unwrap(),
            storage_dir: dir.path().to_path_buf(),
            admin_token: None,
            cookie_secure: false,
            mirror_base: "http://localhost".into(),
            github_client_id: None,
            github_client_secret: None,
            admin_github_uids: Vec::new(),
            debug_token: None,
            debug_github_uids: Vec::new(),
        };
        let app = router(AppState::new(config).unwrap());

        // A panel asset that does not exist answers 404 from the asset handler.
        // Escaped, this request would instead be served the app shell: 200 HTML.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/assets/nothing-here.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "an /assets/ request must reach the asset handler, not the SPA fallback"
        );

        // Same for a pack's static tree, one level deep so only a wildcard can
        // match it. The handler answers the API's error envelope; the fallback
        // answers plain text, so the body says which one replied.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/packs/Ghost/static/_nexira/icon.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").cloned())
                .is_some(),
            "a pack-static request must reach the handler; got {:?}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Ask a one-route service wearing the real compression layer for a body of
    /// `content_type`, and report what it came back encoded as.
    async fn encoding_of(content_type: &'static str) -> Option<String> {
        use axum::body::Body;
        use axum::http::{Request, header};
        use axum::response::IntoResponse;
        use axum::routing::get;
        use tower::ServiceExt;

        // comfortably past the layer's minimum size, and compressible
        let payload = "smrt".repeat(64);
        let app = Router::new()
            .route(
                "/x",
                get(move || async move {
                    ([(header::CONTENT_TYPE, content_type)], payload).into_response()
                }),
            )
            .layer(compression());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/x")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        resp.headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn json_travels_compressed() {
        assert_eq!(
            encoding_of("application/json").await.as_deref(),
            Some("gzip")
        );
    }

    // A jar is a zip: re-compressing it buys nothing and costs a pass over every
    // byte the mirror serves, which is the bulk of its traffic.
    #[tokio::test]
    async fn an_archive_is_served_as_it_lies() {
        assert_eq!(encoding_of("application/java-archive").await, None);
        assert_eq!(encoding_of("application/zip").await, None);
        assert_eq!(encoding_of("application/octet-stream").await, None);
    }

    // The one case where compression would not just waste work but break the
    // feature: a build log delivered a line at a time must not be held back to
    // fill an encoder's buffer.
    #[tokio::test]
    async fn a_live_stream_is_never_compressed() {
        assert_eq!(encoding_of("text/event-stream").await, None);
    }

    // A resumed download splices the answer into a file at the offsets it asked
    // for. Encoding the slice after cutting it would count those offsets in one
    // ruler and deliver bytes measured in another.
    #[tokio::test]
    async fn a_partial_answer_is_never_compressed() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use axum::response::IntoResponse;
        use axum::routing::get;
        use tower::ServiceExt;

        let payload = "smrt".repeat(64);
        let app = Router::new()
            .route(
                "/x",
                get(move || async move {
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [
                            (header::CONTENT_TYPE, "text/plain"),
                            (header::CONTENT_RANGE, "bytes 0-255/1024"),
                        ],
                        payload,
                    )
                        .into_response()
                }),
            )
            .layer(compression());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/x")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.headers().get(header::CONTENT_ENCODING).is_none());
    }

    #[test]
    fn full_router_assembles_without_route_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            bind_addr: std::net::SocketAddr::from_str("127.0.0.1:0").unwrap(),
            storage_dir: dir.path().to_path_buf(),
            admin_token: None,
            cookie_secure: false,
            mirror_base: "http://localhost".into(),
            github_client_id: None,
            github_client_secret: None,
            admin_github_uids: Vec::new(),
            debug_token: None,
            debug_github_uids: Vec::new(),
        };
        let state = AppState::new(config).unwrap();
        let _ = router(state);
    }
}
