//! What each of a pack's jars demands of the loader build, and whether the
//! build the pack pins satisfies it (#164).
//!
//! The loader version is a hand-typed pin; the mods around it move on their own
//! schedule. When one of them declares a floor above the pin -- JEI 19.42
//! wanting `neoforge [21.1.238,)` under a pack pinned to `21.1.234` -- the
//! loader stops before the main menu and names the mod that asked. Nothing in
//! the mirror saw it coming: the loader is present by construction, so its
//! dependency block was dropped as "not a mod", and the version check that does
//! exist only ever compared mods against each other.
//!
//! The window lives inside the jar, which for a Modrinth pin is not on this
//! disk, so this pass is async and does I/O -- unlike [`super::resolve`], which
//! is pure over the registry. Each artifact is read once and remembered
//! (`artifact_loader_req`); a jar is immutable, so the second build of the same
//! pin costs a database row and nothing else.
//!
//! Only a window naming the pack's own loader is judged. A pack on a fork runs
//! artifacts built for its parent (a Forge jar on Cleanroom), and those declare
//! Forge's numbers, which are not the fork's -- comparing them would be
//! arithmetic on two different scales.

use super::modmeta::{self, LoaderReq};
use super::modrinth::Modrinth;
use super::remotezip::{HttpRanges, read_entry};
use super::resolve::{LoaderVersionIssue, ResolveReport};
use crate::domain::{PackConfig, SourceDecl};
use crate::registry::{Registry, queries, semver, upsert};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The manifests a modern jar declares its loader window in, most specific
/// first -- a NeoForge jar carrying both is judged by its own.
const MANIFESTS: &[&str] = &[
    "META-INF/neoforge.mods.toml",
    "META-INF/mods.toml",
    "fabric.mod.json",
];

/// Concurrent artifact reads. Each is two or three small ranged requests
/// against a CDN, and a pack is ~100 artifacts on its first pass; wide enough
/// that the first check is not a coffee break, narrow enough to stay a polite
/// client.
const READ_CONCURRENCY: usize = 6;

/// The outcome of the pass: what is out of window, and how many jars could not
/// be judged (never read, or a version string not plainly comparable). The
/// second number is reported rather than folded into the first -- an unread jar
/// is missing evidence, not a finding.
#[derive(Debug, Clone, Default)]
pub struct LoaderWindowReport {
    pub issues: Vec<LoaderVersionIssue>,
    pub unchecked: usize,
}

impl LoaderWindowReport {
    /// Fold what this pass learned into the resolve report the gate and the
    /// panel read, so there is one shape carrying findings and one place that
    /// counts what could not be judged.
    pub fn apply(self, report: &mut ResolveReport) {
        report.version_windows_unchecked += self.unchecked;
        report.loader_version_issues = self.issues;
    }
}

/// Judge every declared mod of `cfg` against the loader build it pins.
///
/// Never fails the caller: an artifact that cannot be read (upstream down, a
/// server that will not serve ranges, a jar with no modern manifest) is counted
/// unchecked, because a check that turns an outage into a blocked publish would
/// be routed around within a week.
pub async fn loader_windows(
    cfg: &PackConfig,
    storage_root: &Path,
    registry: &Arc<Registry>,
    modrinth: &Arc<Modrinth>,
) -> LoaderWindowReport {
    let declared: Vec<(String, String, SourceDecl)> = cfg
        .mods
        .iter()
        .filter_map(|m| artifact_key(&m.source).map(|k| (m.filename.clone(), k, m.source.clone())))
        .collect();
    if declared.is_empty() {
        return LoaderWindowReport::default();
    }

    let mut known = read_known(registry, &declared).await;
    let misses: Vec<(String, SourceDecl)> = declared
        .iter()
        .filter(|(_, key, _)| !known.contains_key(key))
        .map(|(_, key, source)| (key.clone(), source.clone()))
        .collect();
    if !misses.is_empty() {
        let learned = read_artifacts(misses, storage_root, modrinth).await;
        store(registry, &learned).await;
        known.extend(learned);
    }

    let pack_loader = cfg.loader.name.trim().to_ascii_lowercase();
    let mut report = LoaderWindowReport::default();
    for (filename, key, _) in &declared {
        let Some(reqs) = known.get(key) else {
            report.unchecked += 1; // never read: no evidence either way
            continue;
        };
        for (loader, range) in reqs {
            if *loader != pack_loader {
                continue;
            }
            match semver::in_range(&cfg.loader.version, range) {
                Some(true) => {}
                Some(false) => report.issues.push(LoaderVersionIssue {
                    filename: filename.clone(),
                    loader: loader.clone(),
                    pack_version: cfg.loader.version.clone(),
                    required_range: range.clone(),
                }),
                None => report.unchecked += 1,
            }
        }
    }
    report
        .issues
        .sort_by(|a, b| a.filename.cmp(&b.filename).then(a.loader.cmp(&b.loader)));
    report
}

/// The identity an artifact is remembered under: its hash when the mirror holds
/// the bytes, and the Modrinth version otherwise -- a pin is read before any
/// build has resolved it to a hash, and that reading must still be findable.
fn artifact_key(source: &SourceDecl) -> Option<String> {
    match source {
        SourceDecl::SmrtCache { sha1 } => Some(sha1.clone()),
        SourceDecl::Modrinth { version_id, .. } => Some(format!("modrinth:{version_id}")),
        SourceDecl::CurseForge { file_id, .. } => Some(format!("curseforge:{file_id}")),
        SourceDecl::Github { repo, tag, asset } => Some(format!("github:{repo}/{tag}/{asset}")),
        SourceDecl::SmrtStatic { .. } => None, // not a jar
    }
}

/// What the registry already knows, in one hop off the pool.
async fn read_known(
    registry: &Arc<Registry>,
    declared: &[(String, String, SourceDecl)],
) -> HashMap<String, Vec<(String, String)>> {
    let keys: Vec<String> = declared.iter().map(|(_, k, _)| k.clone()).collect();
    let reg = registry.clone();
    tokio::task::spawn_blocking(move || {
        reg.with_conn(|c| {
            let mut out = HashMap::new();
            for key in keys {
                if let Some(reqs) = queries::artifact_loader_reqs(c, &key)? {
                    out.insert(key, reqs);
                }
            }
            Ok(out)
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("loader-window read task: {e}"))
    .and_then(|r| r)
    .unwrap_or_else(|e| {
        tracing::warn!(error = %format!("{e:#}"), "reading known loader windows failed");
        HashMap::new()
    })
}

/// Remember what was read, so the next check of the same artifact is a query.
/// Best-effort: a write failure costs a re-read, never the check.
async fn store(registry: &Arc<Registry>, learned: &HashMap<String, Vec<(String, String)>>) {
    let rows: Vec<(String, Vec<(String, String)>)> = learned
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let reg = registry.clone();
    let wrote = tokio::task::spawn_blocking(move || {
        let now = upsert::now_rfc3339();
        reg.with_txn(|c| {
            for (key, reqs) in &rows {
                upsert::set_artifact_loader_reqs(c, key, reqs, &now)?;
            }
            Ok(())
        })
    })
    .await;
    match wrote {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "storing loader windows failed"),
        Err(e) => tracing::warn!(error = %e, "loader-window write task failed"),
    }
}

/// Read each artifact's declared windows, a few at a time. An artifact that
/// cannot be read is absent from the result rather than recorded as silent:
/// "we could not look" must not harden into "it demands nothing".
async fn read_artifacts(
    misses: Vec<(String, SourceDecl)>,
    storage_root: &Path,
    modrinth: &Arc<Modrinth>,
) -> HashMap<String, Vec<(String, String)>> {
    let urls = modrinth_file_urls(&misses, modrinth).await;
    let mut out = HashMap::new();
    for chunk in misses.chunks(READ_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (key, source) in chunk {
            let (key, modrinth) = (key.clone(), modrinth.clone());
            let target = match source {
                SourceDecl::SmrtCache { sha1 } => {
                    Target::Local(super::sources::cache_jar_path(storage_root, sha1).ok())
                }
                SourceDecl::Modrinth { version_id, .. } => {
                    Target::Remote(urls.get(version_id).cloned())
                }
                // Reading one needs a download url, and getting that needs the
                // mirror's key, so what a CurseForge pin declares is read at
                // build time rather than here.
                SourceDecl::CurseForge { .. } => continue,
                // A release asset needs no key and its url is derivable, so this
                // one can be read where it lies, the same as a Modrinth pin.
                SourceDecl::Github { repo, tag, asset } => {
                    Target::Remote(super::github::asset_url(repo, tag, asset))
                }
                SourceDecl::SmrtStatic { .. } => continue,
            };
            set.spawn(async move {
                let reqs = match target {
                    Target::Local(Some(path)) => read_local(&path).await,
                    Target::Remote(Some(url)) => read_remote(&modrinth, &url).await,
                    _ => None,
                };
                (key, reqs)
            });
        }
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((key, Some(reqs))) => {
                    out.insert(key, reqs);
                }
                Ok((_, None)) => {}
                Err(e) => tracing::warn!(error = %e, "loader-window read task failed"),
            }
        }
    }
    out
}

enum Target {
    Local(Option<PathBuf>),
    Remote(Option<String>),
}

/// Primary-file URLs for the Modrinth pins among `misses`, in one batched
/// lookup. An unreachable API yields none, and every pin is then unchecked.
async fn modrinth_file_urls(
    misses: &[(String, SourceDecl)],
    modrinth: &Arc<Modrinth>,
) -> HashMap<String, String> {
    let ids: Vec<String> = misses
        .iter()
        .filter_map(|(_, s)| match s {
            SourceDecl::Modrinth { version_id, .. } => Some(version_id.clone()),
            _ => None,
        })
        .collect();
    if ids.is_empty() {
        return HashMap::new();
    }
    match modrinth.versions_by_ids(&ids).await {
        Ok(versions) => versions
            .into_iter()
            .filter_map(|(id, v)| Some((id, v.primary_file()?.url.clone())))
            .collect(),
        Err(e) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "modrinth version lookup failed; the loader windows of pinned mods stay unchecked"
            );
            HashMap::new()
        }
    }
}

/// A cached jar, read off the disk the mirror already holds it on.
async fn read_local(path: &Path) -> Option<Vec<(String, String)>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let meta = tokio::task::spawn_blocking(move || modmeta::read_mod_meta(&bytes))
        .await
        .ok()?;
    Some(flatten(&meta.loader_reqs))
}

/// A jar on someone else's server, read by range: two or three requests for a
/// file the mirror otherwise has no reason to download.
async fn read_remote(modrinth: &Arc<Modrinth>, url: &str) -> Option<Vec<(String, String)>> {
    let opened = match HttpRanges::open(modrinth.clone(), url).await {
        Ok(Some(src)) => src,
        Ok(None) => {
            tracing::debug!(
                url,
                "no range support; the jar's loader window stays unread"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(url, error = %format!("{e:#}"), "range probe failed");
            return None;
        }
    };
    let read = tokio::task::spawn_blocking(move || read_entry(&opened, MANIFESTS)).await;
    match read {
        Ok(Ok(Some((name, bytes)))) => {
            let meta = if name.ends_with(".json") {
                modmeta::parse_fabric_json(&bytes)
            } else {
                modmeta::parse_mods_toml(&String::from_utf8_lossy(&bytes))
            };
            Some(flatten(&meta.loader_reqs))
        }
        // no modern manifest is a real answer: a 1.12-era jar declares no
        // loader build, and re-reading it every check would be the waste this
        // whole path exists to avoid
        Ok(Ok(None)) => Some(Vec::new()),
        Ok(Err(e)) => {
            tracing::warn!(url, error = %format!("{e:#}"), "reading the remote jar failed");
            None
        }
        Err(e) => {
            tracing::warn!(url, error = %e, "remote jar read task failed");
            None
        }
    }
}

fn flatten(reqs: &[LoaderReq]) -> Vec<(String, String)> {
    reqs.iter()
        .map(|r| (r.loader.to_ascii_lowercase(), r.range.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeclaredMod, LoaderSpec, PackTier, Visibility};

    fn cfg(loader: &str, version: &str, mods: Vec<DeclaredMod>) -> PackConfig {
        PackConfig {
            pack_id: "Create".into(),
            display_name: "Create".into(),
            tagline: String::new(),
            minecraft_version: "1.21.1".into(),
            loader: LoaderSpec {
                name: loader.into(),
                version: version.into(),
            },
            java_major: 21,
            version: None,
            tags: vec![],
            featured: false,
            mods,
            assets: vec![],
            auth: None,
            pack_meta: Default::default(),
            owner: 0,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    fn cached(filename: &str, sha1: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha1.into() },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    /// A registry that already knows what a jar declares, so the pass has
    /// nothing to fetch -- the state every check after the first is in.
    fn registry_knowing(rows: &[(&str, &[(&str, &str)])]) -> Arc<Registry> {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        r.with_conn_mut(|c| {
            for (key, reqs) in rows {
                let reqs: Vec<(String, String)> = reqs
                    .iter()
                    .map(|(l, v)| (l.to_string(), v.to_string()))
                    .collect();
                upsert::set_artifact_loader_reqs(c, key, &reqs, "2026-08-01T00:00:00Z")?;
            }
            Ok(())
        })
        .unwrap();
        r
    }

    // The real thing, end to end: the jar this check exists for, read out of
    // Modrinth's CDN by range. Ignored by default like the corpus test -- it
    // needs the network and pins an upstream file -- and run by hand when the
    // remote read is touched: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "reads a jar from Modrinth"]
    async fn the_window_of_a_pinned_jar_is_read_off_the_network() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        let modrinth = Arc::new(Modrinth::new().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let jei = DeclaredMod {
            source: SourceDecl::Modrinth {
                project_id: "u6dRKJwZ".into(),
                version_id: "sc43sMLj".into(), // JEI 19.42.0.385
            },
            ..cached("jei.jar", "unused")
        };
        let report = loader_windows(
            &cfg("neoforge", "21.1.234", vec![jei.clone()]),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert_eq!(report.issues.len(), 1, "{report:?}");
        assert_eq!(report.issues[0].required_range, "[21.1.238,)");
        assert_eq!(report.unchecked, 0);

        // and the second pass answers from the registry, with the network
        // pointed somewhere that refuses instantly
        let offline = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let again = loader_windows(
            &cfg("neoforge", "21.1.248", vec![jei]),
            dir.path(),
            &r,
            &offline,
        )
        .await;
        assert!(again.issues.is_empty(), "{again:?}");
        assert_eq!(again.unchecked, 0, "the reading was remembered");
    }

    // The crash that started this: the pack pins 21.1.234 and JEI's jar says
    // [21.1.238,). The finding names the jar, the pin and the window -- the
    // crash log named only the mod that asked.
    #[tokio::test]
    async fn a_pin_below_a_declared_floor_is_a_finding() {
        let r = registry_knowing(&[("jei_sha", &[("neoforge", "[21.1.238,)")])]);
        let modrinth = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let dir = tempfile::tempdir().unwrap();

        let report = loader_windows(
            &cfg("neoforge", "21.1.234", vec![cached("jei.jar", "jei_sha")]),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert_eq!(report.issues.len(), 1, "{report:?}");
        assert_eq!(report.issues[0].filename, "jei.jar");
        assert_eq!(report.issues[0].required_range, "[21.1.238,)");
        assert_eq!(report.issues[0].pack_version, "21.1.234");
        assert_eq!(report.unchecked, 0);

        // and the same pack on the build that satisfies it is clean
        let ok = loader_windows(
            &cfg("neoforge", "21.1.248", vec![cached("jei.jar", "jei_sha")]),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert!(ok.issues.is_empty(), "{ok:?}");
    }

    // A fork runs its parent's artifacts, and those declare the parent's
    // numbers: Cleanroom 0.2.3 against Forge's `[14.23,)` is not a comparison,
    // and answering it either way would be invented.
    #[tokio::test]
    async fn a_window_for_another_loader_is_not_judged() {
        let r = registry_knowing(&[("forge_sha", &[("forge", "[14.23.5.2860,)")])]);
        let modrinth = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let report = loader_windows(
            &cfg("cleanroom", "0.2.3", vec![cached("mod.jar", "forge_sha")]),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert!(report.issues.is_empty(), "{report:?}");
        assert_eq!(report.unchecked, 0, "not judged is not unchecked");
    }

    // A jar the mirror has never read leaves the question open, and an
    // unreadable one must not turn an outage into a blocked publish: both count
    // as unchecked, neither is a finding.
    #[tokio::test]
    async fn what_cannot_be_read_is_counted_not_flagged() {
        let r = Arc::new(Registry::open_in_memory().unwrap());
        // port 1 refuses instantly: upstream down, without the wait
        let modrinth = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let report = loader_windows(
            &cfg(
                "neoforge",
                "21.1.234",
                vec![
                    cached("gone.jar", &"a".repeat(40)),
                    DeclaredMod {
                        source: SourceDecl::Modrinth {
                            project_id: "u6dRKJwZ".into(),
                            version_id: "sc43sMLj".into(),
                        },
                        ..cached("jei.jar", "unused")
                    },
                ],
            ),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert!(report.issues.is_empty(), "{report:?}");
        assert_eq!(report.unchecked, 2, "both jars stayed unread");
    }

    // A jar that declares nothing is a real answer and is remembered as one --
    // otherwise every check re-reads every silent jar in the pack.
    #[tokio::test]
    async fn a_silent_jar_is_remembered_as_silent() {
        let r = registry_knowing(&[("quiet_sha", &[])]);
        let modrinth = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let report = loader_windows(
            &cfg(
                "neoforge",
                "21.1.234",
                vec![cached("quiet.jar", "quiet_sha")],
            ),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert!(report.issues.is_empty());
        assert_eq!(
            report.unchecked, 0,
            "reading it and finding nothing is an answer, not a gap"
        );
    }

    // A window this comparison cannot read (a classifier suffix on either side)
    // is counted, never guessed at -- the rule the version comparison has held
    // to since it started flagging anything.
    #[tokio::test]
    async fn an_incomparable_window_is_unchecked() {
        let r = registry_knowing(&[("odd_sha", &[("neoforge", "[21.1.238-beta,)")])]);
        let modrinth = Arc::new(Modrinth::with_base("http://127.0.0.1:1").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let report = loader_windows(
            &cfg("neoforge", "21.1.234", vec![cached("odd.jar", "odd_sha")]),
            dir.path(),
            &r,
            &modrinth,
        )
        .await;
        assert!(report.issues.is_empty(), "{report:?}");
        assert_eq!(report.unchecked, 1);
    }
}
