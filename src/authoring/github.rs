//! GitHub release assets as a pin source.
//!
//! A release asset has a public, derivable URL, so a pack can name one and the
//! launcher fetches it straight from whoever published it: no key, no copy on
//! this mirror, and a mod whose author releases nowhere else stops having to be
//! self-hosted to be packable.
//!
//! What GitHub does not publish is a content hash, and a manifest entry is
//! nothing without one. So the first build that meets an asset reads it once to
//! find out, and the answer is kept. An asset under a tag does not change in
//! practice, so every later build of every pack that names it is one query.

use super::modrinth::Modrinth;
use crate::registry::{Registry, queries, upsert};
use anyhow::{Context, Result, anyhow};
use std::sync::Arc;

/// What a pin turned out to name.
#[derive(Debug)]
pub(super) struct Asset {
    pub sha1: String,
    pub size: u64,
    /// Where the launcher downloads it from, which is where it came from.
    pub url: String,
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
pub fn asset_url(repo_in: &str, tag_in: &str, asset_in: &str) -> Option<String> {
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

/// Resolve a pin to the bytes it names, reading the asset only the first time.
///
/// The download is the expensive half and it is also the honest one: nothing
/// else can say what an asset's hash is. Recording the answer is best-effort,
/// because a write that fails costs a re-read on the next build and never the
/// build in hand.
pub(super) async fn resolve(
    repo: &str,
    tag: &str,
    asset: &str,
    modrinth: &Modrinth,
    registry: &Arc<Registry>,
) -> Result<Asset> {
    let url = asset_url(repo, tag, asset).ok_or_else(|| {
        anyhow!(
            "github pin {repo}@{tag}/{asset} is not a release asset this mirror will fetch: \
             repo must be owner/name, and tag and asset must each be one path segment"
        )
    })?;

    let known = {
        let (repo, tag, asset) = (repo.to_string(), tag.to_string(), asset.to_string());
        registry
            .read(move |c| queries::github_asset(c, &repo, &tag, &asset))
            .await
            .unwrap_or(None)
    };
    if let Some((sha1, size)) = known {
        return Ok(Asset { sha1, size, url });
    }

    let bytes = modrinth
        .fetch_bytes(&url)
        .await
        .with_context(|| format!("fetching github asset {url}"))?;
    let sha1 = super::sources::sha1_hex(&bytes);
    let size = bytes.len() as u64;
    drop(bytes);

    let (r, t, a, s) = (
        repo.to_string(),
        tag.to_string(),
        asset.to_string(),
        sha1.clone(),
    );
    let reg = registry.clone();
    let wrote = tokio::task::spawn_blocking(move || {
        let now = upsert::now_rfc3339();
        reg.with_txn(|c| upsert::set_github_asset(c, &r, &t, &a, &s, size, &now))
    })
    .await;
    match wrote {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %format!("{e:#}"), url, "recording the asset failed"),
        Err(e) => tracing::warn!(error = %e, url, "asset-record task failed"),
    }

    Ok(Asset { sha1, size, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_repo_and_simple_names_build_a_download_url() {
        assert_eq!(
            asset_url("Kitty-Hivens/open-smrt-network", "v1.2.3", "osn-1.12.2.jar"),
            Some(
                "https://github.com/Kitty-Hivens/open-smrt-network/releases/download/v1.2.3/osn-1.12.2.jar"
                    .into()
            )
        );
    }

    #[test]
    fn a_pasted_url_is_normalized() {
        // a pasted browser URL (scheme + host, trailing .git/slash) still resolves
        for repo in [
            "https://github.com/owner/repo",
            "github.com/owner/repo/",
            "owner/repo.git",
        ] {
            assert_eq!(
                asset_url(repo, "v1", "a.jar"),
                Some("https://github.com/owner/repo/releases/download/v1/a.jar".into()),
                "repo {repo:?}"
            );
        }
    }

    #[test]
    fn rich_asset_names_are_percent_encoded() {
        // spaces / parens / plus -- common in real release assets -- are encoded,
        // not rejected
        let url = asset_url("o/r", "1.0+build5", "Cool Mod (1.12.2).jar").unwrap();
        assert_eq!(
            url,
            "https://github.com/o/r/releases/download/1.0%2Bbuild5/Cool%20Mod%20(1.12.2).jar"
        );
    }

    #[test]
    fn unsafe_inputs_are_refused() {
        // repo must be exactly owner/name
        assert!(asset_url("owner/repo/extra", "v1", "a.jar").is_none());
        assert!(asset_url("justowner", "v1", "a.jar").is_none());
        // tag/asset can't add path depth or traverse
        assert!(asset_url("o/r", "v1/x", "a.jar").is_none());
        assert!(asset_url("o/r", "v1", "sub/a.jar").is_none());
        assert!(asset_url("o/r", "..", "a.jar").is_none());
        assert!(asset_url("o/r", "v1", "").is_none());
    }

    /// The point of the table: an asset the mirror has already read costs a
    /// query, with the network pointed somewhere that refuses instantly.
    #[tokio::test]
    async fn a_known_asset_resolves_without_a_download() {
        const NOW: &str = "2026-09-15T00:00:00Z";
        let r = Arc::new(Registry::open_in_memory().unwrap());
        r.with_conn_mut(|c| {
            upsert::set_github_asset(c, "o/r", "v1", "a.jar", &"a".repeat(40), 4242, NOW)
        })
        .unwrap();
        let offline = Modrinth::with_base("http://127.0.0.1:1").unwrap();
        let got = resolve("o/r", "v1", "a.jar", &offline, &r).await.unwrap();
        assert_eq!(got.sha1, "a".repeat(40));
        assert_eq!(got.size, 4242);
        assert_eq!(
            got.url, "https://github.com/o/r/releases/download/v1/a.jar",
            "the manifest still points at the publisher"
        );
    }

    /// A pin this mirror will not fetch is refused before any request is made,
    /// and says which of the three fields is the problem.
    #[tokio::test]
    async fn a_pin_that_is_not_a_release_asset_is_refused_without_asking() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let offline = Modrinth::with_base("http://127.0.0.1:1").unwrap();
        let err = format!(
            "{:#}",
            resolve("o/r", "v1", "../../etc/passwd", &offline, &r)
                .await
                .unwrap_err()
        );
        assert!(err.contains("one path segment"), "{err}");
    }
}
