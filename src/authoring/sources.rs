//! Source resolution + artifact I/O. Turns a declared source into a wire
//! `ModEntry` / `AssetEntry` (Modrinth lookup or local cache read), and holds
//! the cache/static read-write-URL helpers the build and bootstrap passes
//! share. Internal to the authoring layer.

use super::modrinth::{Modrinth, Version as MrVersion};
use crate::domain::{AssetEntry, DeclaredAsset, DeclaredMod, ModEntry, Source, SourceDecl};
use crate::registry::{Registry, queries};
use crate::storage::{cache_jar_path_in, is_safe_rel_path, sha1_shard};
use anyhow::{Context, Result, anyhow, bail};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process temp-file sequence so concurrent writers to the same target use
/// distinct temp files instead of colliding on one shared `.tmp`.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub(super) struct ModrinthCache {
    inner: tokio::sync::Mutex<HashMap<(String, String), MrVersion>>,
}

impl ModrinthCache {
    pub(super) async fn get_or_fetch(
        &self,
        modrinth: &Modrinth,
        project_id: &str,
        version_id: &str,
    ) -> Result<MrVersion> {
        let key = (project_id.to_string(), version_id.to_string());
        if let Some(v) = self.inner.lock().await.get(&key) {
            return Ok(v.clone());
        }
        let v = modrinth.project_version(project_id, version_id).await?;
        self.inner.lock().await.insert(key, v.clone());
        Ok(v)
    }
}

pub(super) async fn resolve_mod(
    decl: &DeclaredMod,
    storage: &Path,
    mirror_base: &str,
    modrinth: &Modrinth,
    cache: &ModrinthCache,
    registry: &Arc<Registry>,
    fell_back: &mut Vec<String>,
) -> Result<ModEntry> {
    // filename lands in the manifest and the launcher writes mods/<filename>.
    // Reject traversal (any '/', '\\', leading dot, or empty) but keep the broad
    // jar-name charset -- real mod filenames carry brackets, spaces, plus, etc.
    if decl.filename.is_empty()
        || decl.filename.starts_with('.')
        || decl.filename.contains('/')
        || decl.filename.contains('\\')
    {
        bail!("mod filename {:?} is not a safe filename", decl.filename);
    }
    let (sha1, size_bytes, source) = match &decl.source {
        SourceDecl::Modrinth {
            project_id,
            version_id,
        } => {
            // The network is asked first: it is authoritative, and a version
            // re-uploaded upstream must not be built from stale numbers. Only
            // when it cannot answer does the registry stand in -- with what the
            // harvest recorded for this exact version id, which is the same file
            // the build would have downloaded. A pack that has been built before
            // therefore keeps building through an outage (#57); one naming a
            // version the mirror has never seen still fails, and says so.
            match cache.get_or_fetch(modrinth, project_id, version_id).await {
                Ok(v) => {
                    let f = v.primary_file().ok_or_else(|| {
                        anyhow!(
                            "Modrinth version {project_id}/{version_id} ships no file -- \
                             upstream published the version without a jar; pin another one"
                        )
                    })?;
                    (
                        f.hashes.sha1.clone(),
                        f.size,
                        Source::Modrinth {
                            project_id: project_id.clone(),
                            version_id: version_id.clone(),
                        },
                    )
                }
                Err(upstream) => {
                    let known = {
                        let vid = version_id.clone();
                        registry
                            .read(move |c| queries::modrinth_file_by_version_id(c, &vid))
                            .await
                            .unwrap_or(None)
                    };
                    let Some((sha1, size)) = known else {
                        return Err(upstream).with_context(|| {
                            format!(
                                "resolving Modrinth mod {} -- and the registry has no record of \
                                 version {version_id}, so there is nothing to build it from",
                                decl.filename
                            )
                        });
                    };
                    fell_back.push(decl.filename.clone());
                    (
                        sha1,
                        size,
                        Source::Modrinth {
                            project_id: project_id.clone(),
                            version_id: version_id.clone(),
                        },
                    )
                }
            }
        }
        SourceDecl::SmrtCache { sha1 } => {
            let path = cache_jar_path(storage, sha1)?;
            let meta = tokio::fs::metadata(&path).await.with_context(|| {
                format!(
                    "cache jar {} not found for mod {}",
                    path.display(),
                    decl.filename
                )
            })?;
            (
                sha1.clone(),
                meta.len(),
                Source::SmrtCache {
                    url: cache_url(mirror_base, sha1),
                },
            )
        }
        SourceDecl::SmrtStatic { .. } => {
            bail!(
                "mod {} uses smrt_static source -- mods must be modrinth or smrt_cache",
                decl.filename
            );
        }
    };

    Ok(ModEntry {
        filename: decl.filename.clone(),
        sha1,
        size_bytes,
        // required is derived from the dependency graph at build time (a mod another
        // present mod hard-depends on), never hand-set -- start it false here.
        required: false,
        default_enabled: decl.default_enabled,
        source,
        display: decl.display.clone(),
        slug: decl.slug.clone(),
    })
}

pub(super) async fn resolve_asset(
    decl: &DeclaredAsset,
    pack_id: &str,
    storage: &Path,
    mirror_base: &str,
    modrinth: &Modrinth,
    cache: &ModrinthCache,
) -> Result<AssetEntry> {
    // dest lands verbatim in the published manifest and a launcher places files
    // at it. Reject traversal here -- the single choke point every asset
    // (config-authored, curator extras, generated) funnels through at build.
    if !is_safe_rel_path(&decl.dest) {
        bail!("asset dest {:?} is not a safe relative path", decl.dest);
    }
    let (sha1, size_bytes, source) = match &decl.source {
        SourceDecl::Modrinth {
            project_id,
            version_id,
        } => {
            let v = cache
                .get_or_fetch(modrinth, project_id, version_id)
                .await
                .with_context(|| format!("resolving Modrinth asset {}", decl.dest))?;
            let f = v.primary_file().ok_or_else(|| {
                anyhow!(
                    "Modrinth version {project_id}/{version_id} ships no file -- \
                     upstream published the version without a jar; pin another one"
                )
            })?;
            (
                f.hashes.sha1.clone(),
                f.size,
                Source::Modrinth {
                    project_id: project_id.clone(),
                    version_id: version_id.clone(),
                },
            )
        }
        SourceDecl::SmrtStatic { rel_path } => {
            let path = static_asset_path(storage, pack_id, rel_path)?;
            let bytes = tokio::fs::read(&path).await.with_context(|| {
                format!(
                    "static asset {} not found for {}",
                    path.display(),
                    decl.dest
                )
            })?;
            let size = bytes.len() as u64;
            let sha = sha1_hex(&bytes);
            (
                sha,
                size,
                Source::SmrtStatic {
                    url: static_url(mirror_base, pack_id, rel_path),
                },
            )
        }
        SourceDecl::SmrtCache { .. } => {
            bail!(
                "asset {} uses smrt_cache source -- assets must be modrinth or smrt_static",
                decl.dest
            );
        }
    };

    Ok(AssetEntry {
        dest: decl.dest.clone(),
        sha1,
        size_bytes,
        required: decl.required,
        source,
        display: decl.display.clone(),
    })
}

pub(super) fn write_to_cache(root: &Path, sha1: &str, bytes: &[u8]) -> Result<()> {
    let path = cache_jar_path_in(root, sha1).ok_or_else(|| anyhow!("invalid sha1: {sha1}"))?;
    if is_removed_sha1(root, sha1) {
        bail!("sha1 {sha1} is on the removed-list (takedown) and cannot be re-ingested");
    }
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("creating cache prefix dir")?;
    }
    let tmp = path.with_extension(format!(
        "jar.tmp.{}.{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(e).context("writing cache jar tmp");
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).context("renaming cache jar");
    }
    Ok(())
}

/// Honor the takedown list on the ingest path too (storage::save_cache_jar does
/// this for direct uploads). removed.txt lives at the storage root.
fn is_removed_sha1(root: &Path, sha1: &str) -> bool {
    match fs::read_to_string(root.join("removed.txt")) {
        Ok(content) => content.lines().any(|line| line.trim() == sha1),
        Err(_) => false,
    }
}

pub(super) fn write_to_static(static_dir: &Path, rel_path: &str, bytes: &[u8]) -> Result<()> {
    // co-locate the traversal guard with the write so no caller can escape static/
    if !is_safe_rel_path(rel_path) {
        bail!("unsafe static rel_path: {rel_path}");
    }
    let path = static_dir.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating static parent dir")?;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(e).context("writing static tmp");
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).context("renaming static");
    }
    Ok(())
}

pub(super) fn cache_jar_path(storage: &Path, sha1: &str) -> Result<PathBuf> {
    cache_jar_path_in(storage, sha1).ok_or_else(|| anyhow!("invalid sha1: {sha1}"))
}

pub(super) fn static_asset_path(storage: &Path, pack_id: &str, rel_path: &str) -> Result<PathBuf> {
    // same guard as the HTTP/write layers, not a weaker bespoke check
    if !is_safe_rel_path(rel_path) {
        bail!("invalid static rel_path: {rel_path}");
    }
    Ok(storage
        .join("packs")
        .join(pack_id)
        .join("static")
        .join(rel_path))
}

pub(super) fn cache_url(base: &str, sha1: &str) -> String {
    // sha1 is hex-only by construction (verified upstream); no encoding
    // needed for path segments here. Same shard as the on-disk layout.
    let base = base.trim_end_matches('/');
    format!("{base}/v1/cache/{}/{sha1}.jar", sha1_shard(sha1))
}

pub(super) fn static_url(base: &str, pack_id: &str, rel_path: &str) -> String {
    // rel_path may contain spaces, parens, plus, comma (storage's
    // validate_rel_path allows them since real resourcepack and
    // shaderpack filenames carry such characters). Manifest URLs are
    // consumed by strict HTTP clients (Java's URI, kotlinx ktor, Rust
    // reqwest) that reject raw spaces with HTTP 400 from nginx or
    // outright parse failures. Percent-encode every segment so the
    // published URL is RFC 3986-compliant; segment boundaries (/)
    // stay unencoded so the path structure survives.
    let base = base.trim_end_matches('/');
    let pack_enc = url_encode_segment(pack_id);
    let rel_enc = rel_path
        .split('/')
        .map(url_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("{base}/v1/packs/{pack_enc}/static/{rel_enc}")
}

/// Percent-encode a single path segment using the RFC 3986 unreserved
/// set plus sub-delims, minus the path-structural ones. Equivalent in
/// scope to JavaScript's `encodeURIComponent` -- safe to drop into any
/// URL position that holds a single segment.
fn url_encode_segment(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    // RFC 3986: pchar = unreserved / pct-encoded / sub-delims / ":" / "@"
    // We additionally encode "/", "?", "#", "[", "]", "&", "=" (would
    // change URL meaning), space (must always encode), and "%" (would
    // collide with already-encoded sequences).
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

pub(super) fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A build must not die because Modrinth is down. The harvest already recorded
    // the sha1 and size of every version the mirror has seen, so the registry can
    // answer for a pinned version the network cannot -- and the build says it did
    // (#57).
    #[tokio::test]
    async fn a_pinned_modrinth_mod_resolves_from_the_registry_when_upstream_is_down() {
        use crate::registry::upsert;
        const NOW: &str = "2026-07-28T00:00:00Z";
        let dir = tempfile::tempdir().unwrap();
        let r = Arc::new(Registry::open_in_memory().unwrap());
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", "jei")], NOW)?;
            upsert::upsert_mod_version(
                c,
                id,
                "4.16.1",
                &["forge"],
                &"a".repeat(40),
                4242,
                Some("jei.jar"),
                None,
                NOW,
            )?;
            upsert::set_mod_version_modrinth(c, &"a".repeat(40), Some("VERSION_ID"), NOW)?;
            Ok(())
        })
        .unwrap();

        // port 1 refuses instantly: an outage without the wait
        let modrinth = Modrinth::with_base("http://127.0.0.1:1").unwrap();
        let decl = DeclaredMod {
            filename: "jei.jar".into(),
            default_enabled: true,
            source: SourceDecl::Modrinth {
                project_id: "PROJ".into(),
                version_id: "VERSION_ID".into(),
            },
            display: None,
            slug: None,
            pulled: false,
        };
        let mut fell_back = Vec::new();
        let entry = resolve_mod(
            &decl,
            dir.path(),
            "https://mirror.example",
            &modrinth,
            &ModrinthCache::default(),
            &r,
            &mut fell_back,
        )
        .await
        .expect("the registry knows this version");

        assert_eq!(entry.sha1, "a".repeat(40), "the harvested hash is used");
        assert_eq!(entry.size_bytes, 4242);
        assert_eq!(
            fell_back,
            vec!["jei.jar".to_string()],
            "the build is told it did not reach upstream"
        );

        // a version the mirror has never seen still fails, and says both reasons
        let unknown = DeclaredMod {
            source: SourceDecl::Modrinth {
                project_id: "PROJ".into(),
                version_id: "NEVER_HARVESTED".into(),
            },
            ..decl
        };
        let err = resolve_mod(
            &unknown,
            dir.path(),
            "https://mirror.example",
            &modrinth,
            &ModrinthCache::default(),
            &r,
            &mut fell_back,
        )
        .await
        .expect_err("nothing to build it from");
        let msg = format!("{err:#}");
        assert!(msg.contains("NEVER_HARVESTED"), "names the version: {msg}");
    }

    #[test]
    fn write_to_cache_refuses_a_removed_sha1() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sha1 = "a".repeat(40);
        // A direct write succeeds while the takedown list is empty...
        write_to_cache(root, &sha1, b"jar").unwrap();
        // ...but once the sha1 is on removed.txt, re-ingest is refused.
        std::fs::write(root.join("removed.txt"), format!("{sha1}\n")).unwrap();
        let err = write_to_cache(root, &sha1, b"jar").unwrap_err();
        assert!(err.to_string().contains("removed-list"), "got {err}");
    }

    #[tokio::test]
    async fn resolve_asset_rejects_traversal_dest() {
        // Every asset funnels through resolve_asset; a config-authored dest with
        // traversal must be refused before it reaches the manifest.
        let decl = DeclaredAsset {
            dest: "../../etc/cron.d/x".into(),
            required: true,
            source: SourceDecl::SmrtStatic {
                rel_path: "ok.png".into(),
            },
            display: None,
        };
        let modrinth = Modrinth::new().unwrap();
        let cache = ModrinthCache::default();
        let err = resolve_asset(
            &decl,
            "pack",
            Path::new("/tmp"),
            "https://m",
            &modrinth,
            &cache,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("safe relative path"), "got {err}");
    }

    #[test]
    fn static_url_percent_encodes_spaces_and_special_chars() {
        let url = static_url(
            "https://smrt.hivens.dev",
            "Industrial",
            "shaderpacks/Chocapic13 V7.1 High.zip",
        );
        assert_eq!(
            url,
            "https://smrt.hivens.dev/v1/packs/Industrial/static/shaderpacks/Chocapic13%20V7.1%20High.zip"
        );
    }

    #[test]
    fn static_url_keeps_segment_boundaries_unencoded() {
        // The "/" between segments stays as path separator, only the
        // segments themselves get encoded. Catches a regression where
        // someone naively percent-encodes the whole rel_path including
        // its slashes.
        let url = static_url("https://m.example", "pack", "a/b c/d.txt");
        assert_eq!(url, "https://m.example/v1/packs/pack/static/a/b%20c/d.txt");
    }

    #[test]
    fn static_url_encodes_parens_and_plus() {
        let url = static_url("https://m.example", "p", "shaderpacks/BSL (v8+).zip");
        // parens stay literal in this set (allowed by RFC 3986 sub-delims
        // and ktor/reqwest parse them fine); plus encodes to %2B because
        // it has special meaning in query strings and some parsers
        // mistranslate it to space.
        assert!(url.contains("BSL%20(v8%2B).zip"), "got {url}");
    }
}
