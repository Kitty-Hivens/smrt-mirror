//! Conditional GET for the JSON surface: every read answers with an `ETag`, and
//! a caller that sends it back gets `304 Not Modified` with no body.
//!
//! This is the cheap half of "has anything changed?". A launcher polling a pack,
//! or a panel view refreshing a listing, otherwise pays for the whole
//! representation to discover it already has it -- and on the read paths that is
//! the entire cost, since the work of building the answer is small next to
//! shipping tens of kilobytes of it. The manifest carries a `fingerprint` for
//! exactly this question, but that only helps a client that already parsed the
//! body it was trying to avoid downloading, and only on the one endpoint that
//! has the field.
//!
//! The tag is the hash of the JSON as this process would have sent it, so it is
//! stable across restarts and across two mirrors serving the same data, and it
//! is weak (`W/`) because it identifies the data rather than one particular
//! encoding of it -- the same answer gzipped and unzipped is the same answer.
//!
//! Where a handler already tags its own answer, that tag stands: the pack
//! config's is the revision a save is checked against, and a content hash in its
//! place would be a different statement wearing the same header. A listing that
//! says when it was read tags itself as well ([`stamped`]): the moment differs on
//! every request, so a hash that included it would never match twice.

use axum::Json;
use axum::body::HttpBody as _;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sha1::{Digest, Sha1};

/// Bodies past this are answered as they are, untagged. Hashing is linear in
/// the body, so a listing large enough to be worth streaming is large enough
/// that hashing it on every request costs more than the round trip it saves.
const MAX_TAGGED: usize = 8 * 1024 * 1024;

/// Tag a JSON read, and answer `304` when the caller already holds it.
///
/// Runs inside compression: the tag is computed over what the handler produced,
/// and a `304` has no body for the encoder to touch.
pub async fn tag_json(req: Request, next: Next) -> Response {
    // Only safe reads: a POST that answers JSON is reporting what it just did,
    // and "you already have this" is the wrong thing to say about an act.
    let readonly = matches!(*req.method(), Method::GET | Method::HEAD);
    let asked_for = req.headers().get(header::IF_NONE_MATCH).cloned();
    let mut resp = next.run(req).await;

    if !readonly || resp.status() != StatusCode::OK || !is_json(&resp) {
        return resp;
    }
    // A handler that tagged its own answer means something specific by the tag:
    // the pack config's is the revision `If-Match` compares a save against, so
    // replacing it with a content hash would make every save look stale. Its tag
    // is honoured rather than overwritten -- and answering `304` from it needs
    // no hashing at all.
    if let Some(own) = resp.headers().get(header::ETAG).cloned() {
        let Ok(tag) = own.to_str().map(str::to_string) else {
            return resp;
        };
        ask_to_revalidate(resp.headers_mut());
        if asked_for.is_some_and(|v| holds(&v, &tag)) {
            return not_modified(resp);
        }
        return resp;
    }
    // An exact size means the whole body is already in memory (every `Json`
    // response is); anything else is a stream, and buffering a stream to hash it
    // would undo the reason it is streamed.
    let (mut parts, body) = resp.into_parts();
    let Some(len) = body.size_hint().exact() else {
        return Response::from_parts(parts, body);
    };
    if len as usize > MAX_TAGGED {
        return Response::from_parts(parts, body);
    }
    let Ok(bytes) = to_bytes(body, MAX_TAGGED).await else {
        return Response::from_parts(parts, Body::empty());
    };

    let tag = weak_tag(&bytes);
    if let Ok(value) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, value);
    }
    ask_to_revalidate(&mut parts.headers);

    if asked_for.is_some_and(|v| holds(&v, &tag)) {
        return not_modified(Response::from_parts(parts, Body::empty()));
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Answer a listing that carries the moment it was read, tagged by everything in
/// it but that moment. `listing` arrives with the stamp blank, and `stamp` fills
/// it in once the tag is taken.
pub fn stamped<T: Serialize>(
    mut listing: T,
    now: String,
    stamp: impl FnOnce(&mut T, String),
) -> Response {
    let tag = serde_json::to_vec(&listing)
        .ok()
        .and_then(|bytes| HeaderValue::from_str(&weak_tag(&bytes)).ok());
    stamp(&mut listing, now);
    let mut resp = Json(listing).into_response();
    if let Some(tag) = tag {
        resp.headers_mut().insert(header::ETAG, tag);
    }
    resp
}

fn weak_tag(bytes: &[u8]) -> String {
    format!("W/\"{}\"", hex::encode(Sha1::digest(bytes)))
}

/// Without this a browser has no reason to revalidate, so the tag it was given
/// never comes back and the 304 path is never taken. `private` because one
/// middleware covers the public catalog and the session-gated panel reads alike,
/// and a shared cache holding the latter would hand one operator's view to the
/// next caller. A handler that has said something about caching already knows
/// better than this default.
fn ask_to_revalidate(headers: &mut HeaderMap) {
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("private, no-cache"));
}

/// The same answer, reduced to "you already have it": the tag and the caching
/// headers stay, the body and the headers describing it go.
fn not_modified(resp: Response) -> Response {
    let (mut parts, _) = resp.into_parts();
    parts.status = StatusCode::NOT_MODIFIED;
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_TYPE);
    Response::from_parts(parts, Body::empty())
}

fn is_json(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

/// Whether an `If-None-Match` value covers `tag`. The header is a list, `*`
/// means "any representation I might have", and the comparison ignores the
/// weakness marker -- which is the whole point of a weak tag.
fn holds(header_value: &HeaderValue, tag: &str) -> bool {
    let Ok(list) = header_value.to_str() else {
        return false;
    };
    let opaque = |s: &str| s.trim().trim_start_matches("W/").trim().to_string();
    let want = opaque(tag);
    list.split(',')
        .any(|candidate| candidate.trim() == "*" || opaque(candidate) == want)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::http::Request as HttpRequest;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/j",
                get(|| async { axum::Json(serde_json::json!({"a": 1})) }),
            )
            // a handler whose tag means something of its own: the pack config
            // answers its revision, which `If-Match` compares a save against
            .route(
                "/own",
                get(|| async {
                    (
                        [(header::ETAG, "\"rev-7\"")],
                        axum::Json(serde_json::json!({"a": 1})),
                    )
                        .into_response()
                }),
            )
            .route(
                "/act",
                post(|| async { axum::Json(serde_json::json!({"a": 1})) }),
            )
            .route(
                "/plain",
                get(|| async { ([(header::CONTENT_TYPE, "text/plain")], "hello").into_response() }),
            )
            .layer(axum::middleware::from_fn(tag_json))
    }

    async fn fetch(uri: &str, if_none_match: Option<&str>) -> Response {
        let mut req = HttpRequest::builder().uri(uri);
        if let Some(v) = if_none_match {
            req = req.header(header::IF_NONE_MATCH, v);
        }
        app()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn etag_of(resp: &Response) -> String {
        resp.headers()
            .get(header::ETAG)
            .expect("a JSON read carries a tag")
            .to_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn a_json_read_is_tagged_and_revalidates() {
        let resp = fetch("/j", None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(etag_of(&resp).starts_with("W/\""));
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-cache"
        );
    }

    #[tokio::test]
    async fn the_same_answer_tags_the_same_way() {
        let first = etag_of(&fetch("/j", None).await);
        let second = etag_of(&fetch("/j", None).await);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn holding_the_tag_costs_no_body() {
        let tag = etag_of(&fetch("/j", None).await);
        let resp = fetch("/j", Some(&tag)).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert!(resp.headers().get(header::CONTENT_TYPE).is_none());
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(body.is_empty());
    }

    // The tag is weak, so a client (or a proxy) that hands it back with the
    // marker stripped -- or alongside tags for other things it holds -- is
    // still recognised as holding this one.
    #[tokio::test]
    async fn a_tag_is_matched_by_what_it_says_not_how_it_is_written() {
        let tag = etag_of(&fetch("/j", None).await);
        let bare = tag.trim_start_matches("W/").to_string();
        assert_eq!(
            fetch("/j", Some(&bare)).await.status(),
            StatusCode::NOT_MODIFIED
        );
        let among_others = format!("\"something-else\", {tag}");
        assert_eq!(
            fetch("/j", Some(&among_others)).await.status(),
            StatusCode::NOT_MODIFIED
        );
        assert_eq!(
            fetch("/j", Some("*")).await.status(),
            StatusCode::NOT_MODIFIED
        );
    }

    #[tokio::test]
    async fn a_stale_tag_gets_the_answer() {
        let resp = fetch("/j", Some("W/\"0000\"")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], br#"{"a":1}"#);
    }

    // A handler's own tag is a statement about the resource, not a hash of the
    // bytes: the pack config's is the revision a save is checked against, so
    // overwriting it here would make every save look stale to the mirror.
    #[tokio::test]
    async fn a_handlers_own_tag_is_left_alone() {
        let resp = fetch("/own", None).await;
        assert_eq!(etag_of(&resp), "\"rev-7\"");
    }

    #[tokio::test]
    async fn a_handlers_own_tag_still_answers_304() {
        let resp = fetch("/own", Some("\"rev-7\"")).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(etag_of(&resp), "\"rev-7\"", "the caller keeps holding it");
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-cache",
            "its own tag is as much a reason to ask again as a computed one"
        );
        let resp = fetch("/own", Some("\"rev-6\"")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn only_reads_are_tagged() {
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/act")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.headers().get(header::ETAG).is_none());
    }

    #[tokio::test]
    async fn a_non_json_answer_is_left_alone() {
        let resp = fetch("/plain", None).await;
        assert!(resp.headers().get(header::ETAG).is_none());
        assert!(resp.headers().get(header::CACHE_CONTROL).is_none());
    }
}
