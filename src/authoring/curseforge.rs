//! CurseForge, asked one question only: whose file is this.
//!
//! The harvest already asks Modrinth that, by sha1, and believes the answer
//! over anything the jar says about itself. For a mod Modrinth does not carry
//! there is no such outside voice, and the registry falls back to the name and
//! version written inside the jar by whoever built it. A relabelled release
//! rewrites exactly those strings, so the fallback believes a version that
//! never existed.
//!
//! A content hash cannot be talked out of the truth, which is why this leg is
//! worth having even though it costs a key. Nothing here fetches a file or
//! decides whether one may be used: identity and admission stay separate, and
//! admission is somebody's decision, not this module's.

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const CURSEFORGE_BASE: &str = "https://api.curseforge.com";
const USER_AGENT: &str = "Kitty-Hivens/smrt-pack (+https://github.com/Kitty-Hivens/smrt)";
/// Fingerprints per call. The endpoint takes many and the whole mirror is a few
/// hundred jars, so this is set for a polite request size rather than for the
/// fewest possible round trips.
const BATCH_SIZE: usize = 200;
const METADATA_TIMEOUT: Duration = Duration::from_secs(45);

/// CurseForge's file fingerprint: murmur2 over the file with the whitespace
/// bytes removed first.
///
/// It is not plain murmur2 and the difference is silent: hash the bytes as they
/// are and every fingerprint comes out wrong, matching nothing, which reads
/// exactly like "this file is published nowhere". The stripped bytes are tab,
/// line feed, carriage return and space, and the length folded into the seed is
/// the length after stripping, not before.
pub fn fingerprint(bytes: &[u8]) -> u32 {
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    let filtered: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, 0x09 | 0x0a | 0x0d | 0x20))
        .collect();

    let mut h: u32 = 1 ^ (filtered.len() as u32);
    // Walked by hand rather than through a chunking iterator: the shape of this
    // loop is the published algorithm's, and keeping it that way is worth more
    // here than a tidier expression of it.
    let mut i = 0usize;
    while i + 4 <= filtered.len() {
        let mut k = u32::from_le_bytes([
            filtered[i],
            filtered[i + 1],
            filtered[i + 2],
            filtered[i + 3],
        ]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
        i += 4;
    }

    let tail = &filtered[i..];
    if tail.len() == 3 {
        h ^= u32::from(tail[2]) << 16;
    }
    if tail.len() >= 2 {
        h ^= u32::from(tail[1]) << 8;
    }
    if !tail.is_empty() {
        h ^= u32::from(tail[0]);
        h = h.wrapping_mul(M);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

/// What CurseForge says a fingerprint belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub project_id: i64,
    pub file_id: i64,
    /// The name the publisher gave the file, which is the one to believe over
    /// the version string inside the jar.
    pub display_name: String,
    pub file_name: String,
    /// Absent when the project's author has turned off third-party
    /// distribution. The mirror may then name the file but may not serve it,
    /// which is the one case where a self-hosted upload is the right answer.
    pub download_url: Option<String>,
    pub game_versions: Vec<String>,
}

pub struct CurseForge {
    http: Client,
    /// API origin; the real one by default, overridable so tests can point at a
    /// mock server without touching the network.
    base: String,
    key: String,
}

impl CurseForge {
    pub fn new(key: String) -> Result<Self> {
        Self::with_base(CURSEFORGE_BASE, key)
    }

    pub fn with_base(base: &str, key: String) -> Result<Self> {
        if key.trim().is_empty() {
            return Err(anyhow!("curseforge api key is empty"));
        }
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .redirect(Policy::limited(5))
            .build()
            .context("curseforge http client")?;
        Ok(Self {
            http,
            base: base.to_string(),
            key,
        })
    }

    /// Batch lookup by fingerprint, chunked. A fingerprint with no match is
    /// simply absent from the returned map, the way the Modrinth sha1 lookup
    /// behaves, so a caller tells "published nowhere" from "the call failed" by
    /// the absence of an `Err`.
    ///
    /// The response's own `unmatchedFingerprints` is not used: it comes back
    /// null rather than empty even when some of the batch did not match, so
    /// what did not match is derived from what was asked.
    pub async fn files_by_fingerprint(&self, fingerprints: &[u32]) -> Result<HashMap<u32, Match>> {
        let mut out = HashMap::new();
        for chunk in fingerprints.chunks(BATCH_SIZE) {
            let resp = self
                .http
                .post(format!("{}/v1/fingerprints", self.base))
                .header("x-api-key", &self.key)
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(METADATA_TIMEOUT)
                .json(&FingerprintsRequest {
                    fingerprints: chunk,
                })
                .send()
                .await
                .context("curseforge fingerprints post")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("curseforge fingerprints HTTP {status}: {body}"));
            }
            let parsed: FingerprintsResponse = resp
                .json()
                .await
                .context("decode curseforge fingerprints")?;
            for m in parsed.data.exact_matches {
                out.insert(
                    m.file.file_fingerprint,
                    Match {
                        project_id: m.id,
                        file_id: m.file.id,
                        display_name: m.file.display_name,
                        file_name: m.file.file_name,
                        download_url: m.file.download_url,
                        game_versions: m.file.game_versions,
                    },
                );
            }
        }
        Ok(out)
    }
}

/// One published file, as the build needs it: what to download, how big it is,
/// and what it should hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub project_id: i64,
    pub file_id: i64,
    pub display_name: String,
    pub file_name: String,
    pub size_bytes: u64,
    /// Lowercase hex, from CurseForge's own hash list. Absent when the project
    /// publishes no sha1 for the file, which a caller has to treat as "cannot
    /// verify" rather than as a mismatch.
    pub sha1: Option<String>,
    /// Absent exactly when the author has turned off third-party distribution.
    /// The file may still be named; it may not be served.
    pub download_url: Option<String>,
}

impl CurseForge {
    /// One file by project and id, which is what a pin names.
    ///
    /// Separate from the fingerprint lookup because it answers the opposite
    /// question: that one asks who owns bytes we already hold, this one asks
    /// where to get bytes we do not.
    pub async fn file(&self, project_id: i64, file_id: i64) -> Result<FileInfo> {
        let resp = self
            .http
            .get(format!(
                "{}/v1/mods/{project_id}/files/{file_id}",
                self.base
            ))
            .header("x-api-key", &self.key)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(METADATA_TIMEOUT)
            .send()
            .await
            .context("curseforge file get")?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!(
                "curseforge has no file {file_id} under project {project_id}"
            ));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("curseforge file HTTP {status}: {body}"));
        }
        let parsed: FileResponse = resp.json().await.context("decode curseforge file")?;
        let f = parsed.data;
        if f.mod_id != project_id {
            return Err(anyhow!(
                "curseforge file {file_id} belongs to project {}, not {project_id}",
                f.mod_id
            ));
        }
        Ok(FileInfo {
            project_id,
            file_id: f.id,
            display_name: f.display_name,
            file_name: f.file_name,
            size_bytes: f.file_length,
            // algo 1 is sha1 in CurseForge's hash list; 2 is md5.
            sha1: f
                .hashes
                .into_iter()
                .find(|h| h.algo == 1)
                .map(|h| h.value.to_ascii_lowercase()),
            download_url: f.download_url,
        })
    }
}

#[derive(Deserialize)]
struct FileResponse {
    data: FileData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileData {
    id: i64,
    mod_id: i64,
    display_name: String,
    file_name: String,
    file_length: u64,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    hashes: Vec<FileHash>,
}

#[derive(Deserialize)]
struct FileHash {
    value: String,
    algo: i64,
}

#[derive(serde::Serialize)]
struct FingerprintsRequest<'a> {
    fingerprints: &'a [u32],
}

#[derive(Deserialize)]
struct FingerprintsResponse {
    data: FingerprintsData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintsData {
    #[serde(default)]
    exact_matches: Vec<ExactMatch>,
}

#[derive(Deserialize)]
struct ExactMatch {
    /// The project id. CurseForge names it `id` at this level and `modId` on
    /// the file, and the two agree.
    id: i64,
    file: MatchFile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchFile {
    id: i64,
    display_name: String,
    file_name: String,
    file_fingerprint: u32,
    /// Null where the author has disabled third-party distribution.
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    game_versions: Vec<String>,
}

/// A one-route HTTP stub, the same shape the dependency-fill tests use, so
/// these exercise the real client over a real socket without a mock-server
/// dependency and without touching CurseForge.
#[cfg(test)]
pub(super) async fn stub(routes: Vec<(String, String)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let body = routes
                    .iter()
                    .find(|(p, _)| path.starts_with(p.as_str()))
                    .map(|(_, b)| b.clone());
                let resp = match body {
                    Some(b) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                        b.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors taken from a reference implementation that was checked against
    /// the live endpoint: two jars from a pack were fingerprinted here and
    /// recognised there, so the shape of the algorithm is not being confirmed
    /// by the same code that produced it.
    #[test]
    fn fingerprint_matches_the_reference_implementation() {
        assert_eq!(fingerprint(b""), 1_540_447_798);
        assert_eq!(fingerprint(b"a"), 626_045_324);
        assert_eq!(fingerprint(b"ab"), 1_692_487_918);
        assert_eq!(fingerprint(b"abc"), 1_621_425_345);
        assert_eq!(fingerprint(b"abcd"), 3_376_380_438);
        assert_eq!(fingerprint(b"hello world"), 2_824_650_221);

        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(fingerprint(&all), 2_094_645_347);
    }

    /// The whole reason this is not plain murmur2. A file that differs only in
    /// whitespace is the same file to CurseForge.
    #[test]
    fn whitespace_is_stripped_before_hashing() {
        assert_eq!(
            fingerprint(b"hello world"),
            fingerprint(b"h e\tl\nl\ro world")
        );
        assert_eq!(fingerprint(b"abc"), fingerprint(b" \t\r\na\nb\tc "));
    }

    #[test]
    fn a_key_that_is_blank_is_refused_rather_than_sent() {
        assert!(CurseForge::new("   ".to_string()).is_err());
    }

    fn file_json(mod_id: i64, url: &str, sha1: Option<&str>) -> String {
        let hashes = match sha1 {
            // algo 2 is md5 and comes first on purpose: the reader has to pick
            // by algorithm rather than by position.
            Some(s) => format!(r#"[{{"value":"d41d8cd9","algo":2}},{{"value":"{s}","algo":1}}]"#),
            None => r#"[{"value":"d41d8cd9","algo":2}]"#.to_string(),
        };
        format!(
            r#"{{"data":{{"id":4242,"modId":{mod_id},"displayName":"Railcraft 12.0.0",
               "fileName":"railcraft-12.0.0.jar","fileLength":9876,
               "downloadUrl":{url},"hashes":{hashes}}}}}"#
        )
    }

    #[tokio::test]
    async fn a_file_lookup_reads_the_sha1_by_algorithm_not_by_position() {
        let base = stub(vec![(
            "/v1/mods/51195/files/4242".into(),
            file_json(
                51195,
                r#""https://edge.forgecdn.net/x.jar""#,
                Some("abc123"),
            ),
        )])
        .await;
        let cf = CurseForge::with_base(&base, "k".into()).unwrap();
        let f = cf.file(51195, 4242).await.unwrap();
        assert_eq!(f.sha1.as_deref(), Some("abc123"));
        assert_eq!(f.size_bytes, 9876);
        assert_eq!(
            f.download_url.as_deref(),
            Some("https://edge.forgecdn.net/x.jar")
        );
        assert_eq!(f.file_name, "railcraft-12.0.0.jar");
    }

    /// The one case where naming a file and serving it come apart.
    #[tokio::test]
    async fn a_file_the_author_will_not_let_us_serve_comes_back_named_but_unlinked() {
        let base = stub(vec![(
            "/v1/mods/300585/files/4242".into(),
            file_json(300585, "null", Some("abc123")),
        )])
        .await;
        let cf = CurseForge::with_base(&base, "k".into()).unwrap();
        let f = cf.file(300585, 4242).await.unwrap();
        assert!(f.download_url.is_none(), "distribution is disallowed");
        assert_eq!(
            f.display_name, "Railcraft 12.0.0",
            "but it can still be named"
        );
    }

    /// A file id is not scoped to a project, so the pair has to be checked
    /// rather than assumed: otherwise a mistyped project would silently pin
    /// whatever that id happens to be.
    #[tokio::test]
    async fn a_file_belonging_to_another_project_is_refused() {
        let base = stub(vec![(
            "/v1/mods/51195/files/4242".into(),
            file_json(999, r#""https://edge.forgecdn.net/x.jar""#, Some("abc123")),
        )])
        .await;
        let cf = CurseForge::with_base(&base, "k".into()).unwrap();
        let err = cf.file(51195, 4242).await.unwrap_err().to_string();
        assert!(
            err.contains("999"),
            "names the project it really belongs to: {err}"
        );
    }

    #[tokio::test]
    async fn a_file_curseforge_does_not_have_says_which_one() {
        let base = stub(vec![]).await;
        let cf = CurseForge::with_base(&base, "k".into()).unwrap();
        let err = cf.file(51195, 4242).await.unwrap_err().to_string();
        assert!(err.contains("4242"), "{err}");
    }
}
