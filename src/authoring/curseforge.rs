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
    let mut chunks = filtered.chunks_exact(4);

    for c in chunks.by_ref() {
        let mut k = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
    }

    let tail = chunks.remainder();
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
}
