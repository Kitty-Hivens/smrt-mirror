use anyhow::{Context, Result, anyhow};
use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use ts_rs::TS;

const MODRINTH_BASE: &str = "https://api.modrinth.com";
const USER_AGENT: &str = "Kitty-Hivens/smrt-pack (+https://github.com/Kitty-Hivens/smrt)";
const BATCH_SIZE: usize = 100;
const METADATA_TIMEOUT: Duration = Duration::from_secs(45);

pub struct Modrinth {
    http: Client,
    /// API origin; the real one by default, overridable so tests can point at
    /// an unreachable or mock server without touching the network.
    base: String,
    /// Project id -> its icon url, and when that was learned. A project's icon
    /// is asked for once per mod the panel draws and changes about never, so
    /// without this the panel spends an upstream round trip per row -- on every
    /// render, for an answer that was already had.
    icons: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

/// How long a remembered icon url is trusted. Long, because the cost of being
/// stale is a picture that changed upstream and takes a while to follow; short
/// enough that it does follow, and that the map cannot hold a deleted project
/// forever.
const ICON_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// Ceiling on remembered projects, so a mirror that browses a lot of Modrinth
/// cannot grow this without bound. Reached, it is emptied rather than evicted
/// cleverly: the next few lookups pay upstream, and it refills with whatever is
/// actually being looked at.
const ICON_MEMO_MAX: usize = 4096;

/// Send a request, absorbing one 429 by waiting out `Retry-After` (capped at
/// two minutes, default 60s when absent) and retrying once. A build resolves
/// one Modrinth version per declared mod, and a burst of those trips the rate
/// limiter often enough that failing the whole build on the first 429 is not
/// acceptable; anything past the single retry is a real upstream fault.
async fn send_with_backoff(req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    // Total deadline per attempt: an upstream that accepts the connection but
    // never answers (a wedged cache layer) must fail the call, not wedge the
    // caller. Applied here rather than on the client so jar downloads via
    // `fetch_bytes` -- which legitimately run long -- stay unbounded.
    let req = req.timeout(METADATA_TIMEOUT);
    let first = req.try_clone().context("clone request")?;
    let resp = first.send().await.context("modrinth send")?;
    if resp.status().as_u16() != 429 {
        return Ok(resp);
    }
    // Cloudflare's 1015 ships Retry-After 0 (or none); a zero wait would retry
    // into the same window, so the floor is 30s regardless.
    let wait = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(30, 120);
    tracing::warn!(wait, "modrinth rate limit hit; backing off once");
    tokio::time::sleep(Duration::from_secs(wait)).await;
    req.send().await.context("modrinth send (retry)")
}

impl Modrinth {
    pub fn new() -> Result<Self> {
        Self::with_base(MODRINTH_BASE)
    }

    pub fn with_base(base: &str) -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .redirect(Policy::limited(5))
            .build()
            .context("modrinth http client")?;
        Ok(Self {
            http,
            base: base.to_string(),
            icons: Mutex::new(HashMap::new()),
        })
    }

    /// Batch lookup by sha1. Modrinth tolerates up to 100 hashes per call;
    /// chunk transparently. Hashes with no match are simply absent from the
    /// returned map -- the caller distinguishes "not on Modrinth" from a hard
    /// API failure by the absence of an Err.
    pub async fn version_files_by_sha1(
        &self,
        hashes: &[String],
    ) -> Result<HashMap<String, Version>> {
        let mut out = HashMap::new();
        for chunk in hashes.chunks(BATCH_SIZE) {
            let body = VersionFilesRequest {
                hashes: chunk,
                algorithm: "sha1",
            };
            let resp = send_with_backoff(
                self.http
                    .post(format!("{}/v2/version_files", self.base))
                    .json(&body),
            )
            .await
            .context("version_files post")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("modrinth version_files HTTP {status}: {body}"));
            }
            let map: HashMap<String, Version> = resp.json().await.context("decode")?;
            out.extend(map);
        }
        Ok(out)
    }

    /// Whether a caller-supplied project handle may be pasted into an upstream
    /// path.
    ///
    /// A handle arrives from a URL segment, and axum hands a single `{param}`
    /// its percent-decoded value -- slashes included. Interpolated straight in,
    /// `..%2F..%2Fv2%2Ftag` becomes `.../v2/project/../../v2/tag`, which the URL
    /// parser then normalises into a different Modrinth endpoint: an
    /// unauthenticated caller shaping the requests the mirror makes to somebody
    /// else's API. Nothing comes back to them -- only an icon url is read out of
    /// the answer -- but the mirror should not be issuing those requests at all.
    ///
    /// Deliberately a rejection of what breaks a path rather than an allowlist
    /// of characters: Modrinth slugs are richer than base62, and a guard that
    /// refuses a real project is a bug of its own.
    fn is_path_handle(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 64
            && s != "."
            && s != ".."
            && !s.contains('/')
            && !s.contains('\\')
            && !s.contains('?')
            && !s.contains('#')
            && !s.chars().any(|c| c.is_control() || c.is_whitespace())
    }

    /// A project's icon URL -- the launcher's `ModIconResolver` runtime
    /// fallback when a manifest entry carries no `display.icon_url`. `None`
    /// when the project has no icon. Slug or numeric id both work.
    pub async fn project_icon(&self, slug_or_id: &str) -> Result<Option<String>> {
        if !Self::is_path_handle(slug_or_id) {
            return Ok(None);
        }
        if let Some(known) = self.remembered_icon(slug_or_id) {
            return Ok(known);
        }
        let icon = self.fetch_project_icon(slug_or_id).await?;
        self.remember_icon(slug_or_id, &icon);
        Ok(icon)
    }

    /// The icon url learned earlier, while it is still fresh.
    fn remembered_icon(&self, slug_or_id: &str) -> Option<Option<String>> {
        let memo = self.icons.lock().ok()?;
        let (learned, url) = memo.get(slug_or_id)?;
        (learned.elapsed() < ICON_TTL).then(|| url.clone())
    }

    fn remember_icon(&self, slug_or_id: &str, icon: &Option<String>) {
        let Ok(mut memo) = self.icons.lock() else {
            return;
        };
        if memo.len() >= ICON_MEMO_MAX {
            memo.clear();
        }
        memo.insert(slug_or_id.to_string(), (Instant::now(), icon.clone()));
    }

    async fn fetch_project_icon(&self, slug_or_id: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .get(format!("{}/v2/project/{slug_or_id}", self.base))
            .send()
            .await
            .context("project get")?;
        let status = resp.status();
        // No such project is not an error: it means there is no icon to show (a
        // project deleted or gone from Modrinth, or a stale id). The caller renders
        // a letter avatar for `None`; only a real upstream fault is an error.
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("modrinth project HTTP {status}: {body}"));
        }
        let p: ProjectIcon = resp.json().await.context("decode project")?;
        Ok(p.icon_url.filter(|s| !s.is_empty()))
    }

    /// Every Minecraft version Modrinth knows, newest first.
    ///
    /// A tag endpoint rather than a Mojang service: this mirror already talks to
    /// Modrinth, and the list a mod site indexes is the list a pack can be built
    /// against, which is the question being asked.
    pub async fn game_versions(&self) -> Result<Vec<GameVersion>> {
        let resp = self
            .http
            .get(format!("{}/v2/tag/game_version", self.base))
            .send()
            .await
            .context("game version tag get")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("modrinth game_version HTTP {status}: {body}"));
        }
        resp.json().await.context("decode game versions")
    }

    /// Batch version lookup by version id, chunked like the sha1 lookup. Ids with
    /// no match are simply absent from the returned map. Lets a pass that needs
    /// the dependencies of many pinned versions cost one request rather than one
    /// per pin.
    pub async fn versions_by_ids(&self, ids: &[String]) -> Result<HashMap<String, Version>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(BATCH_SIZE) {
            let encoded = serde_json::to_string(chunk).context("encode version ids")?;
            let resp = send_with_backoff(
                self.http
                    .get(format!("{}/v2/versions", self.base))
                    .query(&[("ids", encoded.as_str())]),
            )
            .await
            .context("versions get")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("modrinth versions HTTP {status}: {body}"));
            }
            let list: Vec<Version> = resp.json().await.context("decode versions")?;
            for v in list {
                out.insert(v.id.clone(), v);
            }
        }
        Ok(out)
    }

    pub async fn project_version(&self, project_id: &str, version_id: &str) -> Result<Version> {
        let resp = send_with_backoff(self.http.get(format!(
            "{}/v2/project/{project_id}/version/{version_id}",
            self.base
        )))
        .await
        .context("project version get")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("modrinth project version HTTP {status}: {body}"));
        }
        resp.json().await.context("decode")
    }

    /// Lists all published versions of [slug_or_id], newest first.
    /// When [mc_filter] is Some, narrows to versions whose
    /// `game_versions` array contains that exact MC string.
    ///
    /// Search Modrinth for mods matching [query], optionally narrowed to an MC
    /// version. Returns the top hits (project id / slug / title / icon) so the
    /// panel can add a mod without the operator pasting ids.
    pub async fn search(
        &self,
        query: &str,
        mc: Option<&str>,
        project_type: &str,
    ) -> Result<Vec<SearchHit>> {
        // project_type is one of Modrinth's known kinds (mod / resourcepack /
        // shader); the caller picks it so the panel can browse packs, not just mods.
        // build the facets as JSON so an mc/query value carrying quotes or
        // brackets can't reshape the structure sent upstream
        let mut groups: Vec<Vec<String>> = vec![vec![format!("project_type:{project_type}")]];
        if let Some(v) = mc {
            groups.push(vec![format!("versions:{v}")]);
        }
        let facets = serde_json::to_string(&groups).context("encode facets")?;
        let resp = self
            .http
            .get(format!("{}/v2/search", self.base))
            .query(&[
                ("query", query),
                ("facets", facets.as_str()),
                ("limit", "20"),
            ])
            .send()
            .await
            .context("modrinth search get")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("modrinth search HTTP {status}: {body}"));
        }
        let parsed: SearchResponse = resp.json().await.context("decode search")?;
        Ok(parsed.hits)
    }

    /// Slug or numeric project id both work as the URL segment per
    /// the Modrinth API spec.
    pub async fn project_versions(
        &self,
        slug_or_id: &str,
        mc_filter: Option<&str>,
    ) -> Result<Vec<Version>> {
        if !Self::is_path_handle(slug_or_id) {
            anyhow::bail!("{slug_or_id:?} is not a Modrinth project handle");
        }
        let base = format!("{}/v2/project/{slug_or_id}/version", self.base);
        let Some(mc) = mc_filter else {
            return self.versions_at(&base).await;
        };
        // The Modrinth API expects a JSON-encoded array in this
        // query param, then percent-encoded. `["1.12.2"]` becomes
        // `%5B%221.12.2%22%5D`.
        let encoded = format!("[\"{mc}\"]");
        let qp =
            percent_encoding::utf8_percent_encode(&encoded, percent_encoding::NON_ALPHANUMERIC);
        let filtered = format!("{base}?game_versions={qp}");
        // The filtered listing is served by an upstream cache layer that has
        // been observed down (hanging or 500) while the plain listing still
        // answers. Same result set either way -- the fallback just narrows
        // locally from a larger payload.
        match self.versions_at(&filtered).await {
            Ok(v) => Ok(v),
            Err(e) => {
                tracing::warn!(error = %e, project = slug_or_id,
                    "filtered version listing failed; retrying unfiltered");
                let all = self.versions_at(&base).await?;
                Ok(all
                    .into_iter()
                    .filter(|v| v.game_versions.iter().any(|g| g == mc))
                    .collect())
            }
        }
    }

    async fn versions_at(&self, url: &str) -> Result<Vec<Version>> {
        let resp = send_with_backoff(self.http.get(url))
            .await
            .context("project versions get")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("modrinth project versions HTTP {status}: {body}"));
        }
        resp.json().await.context("decode")
    }

    /// Batch project lookup by id/slug for harvest enrichment: title, slug, and
    /// the owning team id (the project object carries no author username -- that
    /// comes from [`team_owners_by_ids`]). Chunked like the sha1 lookup. Ids with
    /// no match are simply absent from the returned map.
    pub async fn projects_by_ids(&self, ids: &[String]) -> Result<HashMap<String, Project>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(BATCH_SIZE) {
            let encoded = serde_json::to_string(chunk).context("encode project ids")?;
            let resp = send_with_backoff(
                self.http
                    .get(format!("{}/v2/projects", self.base))
                    .query(&[("ids", encoded.as_str())]),
            )
            .await
            .context("projects get")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("modrinth projects HTTP {status}: {body}"));
            }
            let list: Vec<Project> = resp.json().await.context("decode projects")?;
            for p in list {
                out.insert(p.id.clone(), p);
            }
        }
        Ok(out)
    }

    /// Batch team lookup -> the owner's username per team id. Modrinth returns one
    /// member array per requested team; the `Owner`-role member is the author we
    /// attribute the mod to. Teams with no owner row are absent from the map.
    pub async fn team_owners_by_ids(&self, team_ids: &[String]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        for chunk in team_ids.chunks(BATCH_SIZE) {
            let encoded = serde_json::to_string(chunk).context("encode team ids")?;
            let resp = self
                .http
                .get(format!("{}/v2/teams", self.base))
                .query(&[("ids", encoded.as_str())])
                .send()
                .await
                .context("teams get")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("modrinth teams HTTP {status}: {body}"));
            }
            // shape: [[member, ...], [member, ...]] -- one inner array per team
            let teams: Vec<Vec<TeamMember>> = resp.json().await.context("decode teams")?;
            for members in teams {
                if let Some(owner) = members
                    .iter()
                    .find(|m| m.role.eq_ignore_ascii_case("owner"))
                {
                    out.insert(owner.team_id.clone(), owner.user.username.clone());
                }
            }
        }
        Ok(out)
    }

    /// Generic GET for artifacts hosted outside Modrinth (GitHub release
    /// assets), reusing the pooled client + UA. Follows redirects.
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        // cap the response so a huge or malicious release asset can't OOM the
        // mirror; GitHub release downloads always send a content-length
        const MAX_BYTES: u64 = 512 * 1024 * 1024;
        let mut resp = self.http.get(url).send().await.context("asset GET")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("asset HTTP {status} for {url}"));
        }
        // content-length is an early reject; a missing or lying header is still
        // bounded by counting the bytes we actually read
        if let Some(len) = resp.content_length()
            && len > MAX_BYTES
        {
            return Err(anyhow!(
                "asset at {url} is {len} bytes, over the {} MiB cap",
                MAX_BYTES / 1024 / 1024
            ));
        }
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.context("asset body")? {
            if buf.len() as u64 + chunk.len() as u64 > MAX_BYTES {
                return Err(anyhow!(
                    "asset at {url} exceeds the {} MiB cap",
                    MAX_BYTES / 1024 / 1024
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    /// How long a remote file is, if the server will serve it by range.
    /// `None` when it will not: without ranges the only way to read one entry
    /// out of a jar is to download the jar, which is the cost this avoids.
    ///
    /// Asked with a one-byte range rather than a HEAD: a CDN may answer HEAD
    /// from an edge that never sees the object, and `Content-Range` on a real
    /// 206 both proves ranges work and carries the length.
    pub async fn ranged_length(&self, url: &str) -> Result<Option<u64>> {
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-0")
            .timeout(METADATA_TIMEOUT)
            .send()
            .await
            .context("range probe")?;
        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Ok(None);
        }
        // "bytes 0-0/1639573" -- the total is what is wanted, and a `*` total
        // (a server that knows the range but not the size) is unusable here
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.trim().parse::<u64>().ok());
        Ok(total)
    }

    /// `len` bytes from `from` of a remote file. Only meaningful against a
    /// server that answered [`ranged_length`]; a server that ignores the range
    /// header would stream the whole file, so a short read is an error rather
    /// than something to work around.
    pub async fn fetch_range(&self, url: &str, from: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let last = from + len - 1;
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={from}-{last}"))
            .timeout(METADATA_TIMEOUT)
            .send()
            .await
            .context("range GET")?;
        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow!(
                "range GET for {url} answered {} rather than 206",
                resp.status()
            ));
        }
        let bytes = resp.bytes().await.context("range body")?;
        if bytes.len() as u64 > len {
            return Err(anyhow!(
                "range GET for {url} answered {} bytes for a {len}-byte window",
                bytes.len()
            ));
        }
        Ok(bytes.to_vec())
    }

    /// Fetch a remote image (e.g. a GitHub avatar) through the pooled client,
    /// returning its bytes and content type. Reuses the shared client so an
    /// image proxy does not open a TLS handshake per request, and caps small --
    /// avatars are tiny, and this bounds what the proxy will pull. A non-image
    /// content type is normalised to `image/png`; the caller serves it `nosniff`.
    pub async fn fetch_image(&self, url: &str) -> Result<(Vec<u8>, String)> {
        const MAX_BYTES: u64 = 8 * 1024 * 1024;
        let resp = self.http.get(url).send().await.context("image GET")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("image HTTP {status} for {url}"));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with("image/"))
            .unwrap_or("image/png")
            .to_string();
        if let Some(len) = resp.content_length()
            && len > MAX_BYTES
        {
            return Err(anyhow!("image at {url} is {len} bytes, over the cap"));
        }
        let bytes = resp.bytes().await.context("image body")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(anyhow!("image at {url} exceeds the cap"));
        }
        Ok((bytes.to_vec(), content_type))
    }
}

#[derive(Serialize)]
struct VersionFilesRequest<'a> {
    hashes: &'a [String],
    algorithm: &'static str,
}

/// One Minecraft version as Modrinth indexes it. `version_type` separates a
/// release from a snapshot, which is the difference between what a pack is
/// normally built against and what almost nothing is mirrored for.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct GameVersion {
    pub version: String,
    pub version_type: String,
    pub date: String,
    /// Modrinth's own marker for a version worth showing first in a picker.
    #[serde(default)]
    pub major: bool,
}

#[derive(Deserialize)]
struct ProjectIcon {
    #[serde(default)]
    icon_url: Option<String>,
}

/// A project as the batch `/v2/projects` lookup returns it -- the subset harvest
/// enrichment reads. `team` is the id used to resolve the author username.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub team: String,
    /// Declared environment flags: `required` | `optional` | `unsupported`
    /// (upstream also ships `unknown`). Authored by the project owner, so they
    /// can be wrong -- the classifier maps them with priority but the resolve
    /// report surfaces disagreements with the bytecode derivation.
    #[serde(default)]
    pub client_side: String,
    #[serde(default)]
    pub server_side: String,
}

#[derive(Deserialize)]
struct TeamMember {
    team_id: String,
    role: String,
    user: TeamUser,
}

#[derive(Deserialize)]
struct TeamUser {
    username: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Project owner's username -- a denormalized convenience Modrinth ships only
    /// on search hits (the project object has no author field). The panel shows it
    /// as a pick-time facet.
    #[serde(default)]
    pub author: String,
    /// Modrinth's category facets, which is where it puts the loaders. The only
    /// loader signal available for a project the mirror has never harvested, so
    /// without it such a hit cannot be judged for fit at all.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Latest MC versions the project supports, as Modrinth reports them.
    #[serde(default)]
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    /// Release channel: `release` | `beta` | `alpha`. Lets the panel filter out
    /// pre-releases so the operator isn't wading through every snapshot.
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    pub files: Vec<VersionFile>,
    /// Project-level deps (required/optional/incompatible/embedded). Additive,
    /// no wire impact; the registry harvest reads these for relation facts.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

/// A Modrinth version dependency. `project_id` is the target (Modrinth
/// namespace); `version_id` pins an exact version when set; `dependency_type` is
/// required|optional|incompatible|embedded. `file_name` is Modrinth's slot for
/// an external dependency (`project_id` null): the file the mod needs that
/// lives outside Modrinth -- the hybrid-resolution key for matching it against
/// the mirror's own cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionFile {
    pub hashes: VersionFileHashes,
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionFileHashes {
    pub sha1: String,
}

impl Version {
    /// Pick the file flagged `primary: true`; if none is, fall back to the
    /// first entry. Matches the wire-spec rule for source resolution.
    pub fn primary_file(&self) -> Option<&VersionFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client pointed at a port nothing listens on: any call that actually
    /// goes upstream fails, which is what makes "it did not go upstream"
    /// something a test can assert.
    fn offline() -> Modrinth {
        Modrinth::with_base("http://127.0.0.1:9").unwrap()
    }

    #[tokio::test]
    async fn an_icon_asked_for_twice_is_fetched_once() {
        let m = offline();
        // nothing remembered yet: the call has to go out, and cannot
        assert!(m.project_icon("create").await.is_err());

        m.remember_icon("create", &Some("https://cdn/icon.png".into()));
        assert_eq!(
            m.project_icon("create").await.unwrap(),
            Some("https://cdn/icon.png".into()),
            "the remembered answer is given without an upstream call"
        );
    }

    // A project with no icon is an answer too, and the expensive part is the
    // asking rather than the picture.
    #[tokio::test]
    async fn a_project_without_an_icon_is_remembered_as_having_none() {
        let m = offline();
        m.remember_icon("iron-chests", &None);
        assert_eq!(m.project_icon("iron-chests").await.unwrap(), None);
    }

    // The handle comes out of a URL segment, and axum hands a single `{param}`
    // its decoded value -- slashes and all. Pasted into the upstream path it
    // would normalise into a different Modrinth endpoint, so an anonymous
    // caller would be choosing which requests the mirror makes to somebody
    // else's API.
    #[tokio::test]
    async fn a_handle_that_would_reshape_the_upstream_path_is_refused() {
        let m = offline();
        for hostile in [
            "../../v2/tag/category",
            "..",
            ".",
            "a/b",
            "a\\b",
            "x?y",
            "x#y",
            "",
        ] {
            assert_eq!(
                m.project_icon(hostile).await.unwrap(),
                None,
                "icon lookup for {hostile:?}"
            );
            assert!(
                m.project_versions(hostile, None).await.is_err(),
                "version listing for {hostile:?}"
            );
        }
        // and a real handle is still a real handle -- the guard refuses what
        // breaks a path, not everything that is not base62
        assert!(Modrinth::is_path_handle("iron-chests"));
        assert!(Modrinth::is_path_handle("P7dR8mSH"));
        assert!(Modrinth::is_path_handle("some.mod_name-2"));
    }

    #[tokio::test]
    async fn the_memo_does_not_grow_without_bound() {
        let m = offline();
        for i in 0..=ICON_MEMO_MAX {
            m.remember_icon(&format!("project-{i}"), &None);
        }
        let held = m.icons.lock().unwrap().len();
        assert!(held <= ICON_MEMO_MAX, "held {held} entries");
    }
}
