//! OpenAPI documentation for the mirror's HTTP API.
//!
//! The spec is derived from the handlers themselves (the `#[utoipa::path]`
//! attributes and `ToSchema` derives on the wire types), so it tracks the code
//! rather than a hand-maintained document. It is served two ways: the raw
//! `/openapi.json` for tooling, and `/docs`, a Scalar reference UI.
//!
//! Scalar's browser bundle is vendored and served from this binary
//! (`/docs/scalar.js`), not loaded from a CDN -- the docs page makes no external
//! request, in keeping with the mirror's self-hosted, no-phone-home stance.

use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use utoipa::OpenApi;

use crate::domain::diff::{DiffEntry, DiffToggle, DiffUpdate, FieldChange, PackDiff};
use crate::domain::manifest::{
    AssetEntry, AuthSpec, BuildChecks, Display, JavaSpec, LoaderSpec, MinecraftSpec, ModEntry,
    PackManifest, Requirement, Source,
};
use crate::domain::pack::{
    CommunityPack, ManifestBuildInfo, ManifestVersionsListing, PackListing, PackSummary, PackTier,
    Visibility,
};
use crate::domain::server::{
    CacheInventory, CacheInventoryEntry, Featured, Health, ServerEntry, ServerListing,
};
use crate::domain::side::PresenceClass;
use crate::domain::version::VersionChannel;
use crate::registry::model::{FileDetail, ModDetail, ModEdge, ModUse, ReleaseRow, VersionRow};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "smrt mirror API",
        version = "1",
        description = "Read and authoring API for the smrt Minecraft mod mirror. \
                       The public surface below is what the launcher and the site \
                       consume; member, operator and debug surfaces are added as \
                       they are annotated."
    ),
    paths(
        crate::http::public::health,
        crate::http::public::list_packs,
        crate::http::public::get_pack_summary,
        crate::http::public::get_latest_manifest,
        crate::http::public::get_manifest_version,
        crate::http::public::list_manifest_versions,
        crate::http::public::get_pack_diff,
        crate::http::public::public_threads,
        crate::http::public::public_thread,
        crate::http::public::public_thread_diff,
        crate::http::public::get_pack_static,
        crate::http::public::list_community,
        crate::http::public::list_servers,
        crate::http::public::get_server,
        crate::http::public::get_featured,
        crate::http::public::get_mod_detail,
        crate::http::public::get_file_detail,
        crate::http::public::get_cache_jar,
        crate::http::public::get_cache_icon,
        crate::http::public::get_cache_inventory,
        crate::http::public::get_user_avatar,
    ),
    components(schemas(
        crate::accounts::Thread,
        crate::accounts::ThreadComment,
        crate::accounts::ThreadView,
        crate::http::admin::CommitDiff,
        crate::authoring::ConfigChange,
        crate::authoring::ChangeGroup,
        crate::authoring::ChangeOp,
        crate::authoring::ChangeField,
        Health,
        PackListing,
        PackSummary,
        PackTier,
        Visibility,
        CommunityPack,
        PackManifest,
        BuildChecks,
        AuthSpec,
        MinecraftSpec,
        LoaderSpec,
        JavaSpec,
        ModEntry,
        AssetEntry,
        Display,
        Requirement,
        Source,
        PresenceClass,
        ManifestVersionsListing,
        ManifestBuildInfo,
        PackDiff,
        FieldChange,
        DiffEntry,
        DiffUpdate,
        DiffToggle,
        VersionChannel,
        ModDetail,
        FileDetail,
        ReleaseRow,
        VersionRow,
        ModEdge,
        ModUse,
        CacheInventory,
        CacheInventoryEntry,
        ServerListing,
        ServerEntry,
        Featured,
    )),
    tags((name = "public", description = "Unauthenticated reads: the launcher catalog, manifests, mods, cache."))
)]
struct ApiDoc;

pub fn router() -> Router {
    Router::new()
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(docs_page))
        .route("/docs/scalar.js", get(scalar_js))
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

// Vendored Scalar standalone bundle -- served locally so the docs page never
// reaches a CDN.
const SCALAR_JS: &str = include_str!("../../vendor/scalar.standalone.js");

async fn scalar_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        SCALAR_JS,
    )
}

// `withDefaultFonts:false` stops Scalar pulling Inter/mono from fonts.scalar.com;
// it falls back to the system stack. The CSP below is the hard guarantee: even if
// the bundle tries to reach fonts.scalar.com or api.scalar.com (its hosted search),
// the browser refuses -- connect-src and font-src are 'self' only, so the docs
// page contacts nothing but this mirror.
const DOCS_HTML: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>smrt mirror API</title>
  </head>
  <body>
    <script
      id="api-reference"
      data-url="/openapi.json"
      data-configuration='{"withDefaultFonts":false,"darkMode":true}'
    ></script>
    <script src="/docs/scalar.js"></script>
  </body>
</html>
"#;

const DOCS_CSP: &str = "default-src 'self'; \
    script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; \
    font-src 'self' data:; \
    connect-src 'self'";

async fn docs_page() -> impl IntoResponse {
    (
        [(header::CONTENT_SECURITY_POLICY, DOCS_CSP)],
        Html(DOCS_HTML),
    )
}

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use utoipa::OpenApi;
    use utoipa::openapi::Deprecated;

    // A client author reads the spec, not the source: a route that is going away
    // has to say so there.
    #[test]
    fn the_server_reads_are_marked_deprecated() {
        let doc = ApiDoc::openapi();
        for path in ["/v1/servers", "/v1/servers/{server_id}"] {
            let op = doc.paths.paths[path]
                .get
                .as_ref()
                .unwrap_or_else(|| panic!("{path} has a GET"));
            assert!(
                matches!(op.deprecated, Some(Deprecated::True)),
                "{path} is deprecated in the spec"
            );
        }
        let live = doc.paths.paths["/v1/packs"].get.as_ref().unwrap();
        assert!(!matches!(live.deprecated, Some(Deprecated::True)));
    }
}
