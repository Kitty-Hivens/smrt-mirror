//! Which builds of a loader a pack can be pinned to (#126).
//!
//! The loader version was typed by hand, and a wrong one is not caught by
//! anything the mirror does -- it travels into the manifest and surfaces as a
//! launcher that will not start. Every loader publishes its builds; this holds a
//! copy of each list and answers from it.
//!
//! Held rather than proxied, for the reasons the Minecraft list is (see
//! `versions.rs`), and on the same freshness window: an editor must open without
//! four external services being up, and when one is down the honest answer is
//! what was last known.
//!
//! Forge is fetched whole rather than from its promotions file. Promotions is
//! 4 kB against 210 kB and names only the latest and recommended build per
//! Minecraft version -- and this mirror's own flagship pack is pinned to
//! `14.23.5.2860`, which is neither. A picker built from promotions could not
//! offer the version the deployment already runs. The markers are still worth
//! having, so they are read from promotions and applied to the full list:
//! *recommended* is a fact a typed field cannot carry, and it is the difference
//! between a build that works and one that is merely newest.

use super::modrinth::Modrinth;
use crate::storage::Storage;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ts_rs::TS;

const FORGE_MAVEN: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions/loader";
const QUILT_META: &str = "https://meta.quiltmc.org/v3/versions/loader";

/// One build of a loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LoaderBuild {
    pub version: String,
    /// The Minecraft version this build is for, where the loader ties them
    /// together (Forge, NeoForge). Absent for loaders whose builds are
    /// independent of it -- Fabric and Quilt publish one loader for all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub minecraft: Option<String>,
    /// Upstream's own "use this one" marker. Forge publishes it explicitly;
    /// Fabric and Quilt call it stable.
    #[serde(default)]
    pub recommended: bool,
    /// Newest for its Minecraft version.
    #[serde(default)]
    pub latest: bool,
}

/// A loader's builds, as the mirror last knew them.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LoaderVersions {
    pub loader: String,
    pub builds: Vec<LoaderBuild>,
    pub fetched_at: String,
    #[ts(type = "number")]
    pub fetched_unix: u64,
    /// Past its freshness window: a refresh is in flight behind this answer, or
    /// upstream is not answering.
    #[serde(default)]
    pub stale: bool,
}

/// Loaders with a published build list. A pack may name others -- a fork the
/// mirror knows through the registry's loader chain -- and for those the field
/// stays what it always was: free text, with nothing to offer.
pub fn is_known(loader: &str) -> bool {
    matches!(
        loader.trim().to_ascii_lowercase().as_str(),
        "forge" | "neoforge" | "fabric" | "quilt"
    )
}

/// The builds for a loader, from the cache when it is fresh and upstream when
/// it is not. An upstream failure yields the last known list, marked stale --
/// an empty picker sends the operator back to typing, which is the thing being
/// replaced.
pub async fn loader_versions(
    storage: &Arc<Storage>,
    modrinth: &Arc<Modrinth>,
    loader: &str,
) -> Result<LoaderVersions> {
    let loader = loader.trim().to_ascii_lowercase();
    if !is_known(&loader) {
        bail!("no published build list for loader {loader:?}");
    }
    let key = format!("loader-{loader}");
    let cached: Option<LoaderVersions> = storage.load_meta_list(&key).await.ok().flatten();
    if let Some(list) = cached {
        if super::versions::is_fresh(list.fetched_unix) {
            return Ok(list);
        }
        // Served now, refreshed behind. Forge's metadata is 210 kB, and having
        // whoever opens the editor first after six hours pay for that download
        // is an economy taken out of someone else's afternoon.
        let (storage, modrinth, loader) = (storage.clone(), modrinth.clone(), loader.clone());
        tokio::spawn(async move {
            if let Err(e) = refresh(&storage, &modrinth, &loader).await {
                tracing::warn!(loader, error = %format!("{e:#}"), "refreshing the loader build list failed");
            }
        });
        return Ok(LoaderVersions {
            stale: true,
            ..list
        });
    }

    // Nothing cached: no older answer exists to hand back, so this one waits.
    refresh(storage, modrinth, &loader)
        .await
        .context("no cached loader build list to fall back on")
}

/// Ask upstream and write what comes back.
async fn refresh(
    storage: &Arc<Storage>,
    modrinth: &Arc<Modrinth>,
    loader: &str,
) -> Result<LoaderVersions> {
    let builds = fetch(modrinth, loader).await?;
    let now = super::versions::unix_now();
    let fresh = LoaderVersions {
        loader: loader.to_string(),
        builds,
        fetched_at: super::versions::rfc3339(now),
        fetched_unix: now,
        stale: false,
    };
    if let Err(e) = storage
        .save_meta_list(&format!("loader-{loader}"), &fresh)
        .await
    {
        tracing::warn!(loader, error = %e, "caching the loader build list failed");
    }
    Ok(fresh)
}

async fn fetch(modrinth: &Arc<Modrinth>, loader: &str) -> Result<Vec<LoaderBuild>> {
    // The HTTP client is the shared one: these are plain GETs, and a second
    // pooled client for four URLs would be a second thing to configure.
    match loader {
        "forge" => {
            let xml =
                String::from_utf8_lossy(&modrinth.fetch_bytes(FORGE_MAVEN).await?).into_owned();
            let promos = modrinth.fetch_bytes(FORGE_PROMOTIONS).await.ok();
            Ok(parse_forge(&xml, promos.as_deref()))
        }
        "neoforge" => {
            let body = modrinth.fetch_bytes(NEOFORGE_API).await?;
            parse_neoforge(&body)
        }
        "fabric" => parse_fabric(&modrinth.fetch_bytes(FABRIC_META).await?),
        "quilt" => parse_fabric(&modrinth.fetch_bytes(QUILT_META).await?),
        other => bail!("no source for loader {other:?}"),
    }
}

/// Forge publishes `<minecraft>-<forge>` per entry, newest last. Scanned rather
/// than parsed as XML: one tag is wanted out of a document whose shape has not
/// moved in a decade, and an XML crate for that is more to maintain than the
/// four lines it replaces.
fn parse_forge(xml: &str, promotions: Option<&[u8]>) -> Vec<LoaderBuild> {
    let promos: std::collections::HashMap<String, String> = promotions
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.get("promos").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default();

    let mut out: Vec<LoaderBuild> = Vec::new();
    for chunk in xml.split("<version>").skip(1) {
        let Some(raw) = chunk.split("</version>").next() else {
            continue;
        };
        let Some((mc, version)) = raw.trim().split_once('-') else {
            continue;
        };
        out.push(LoaderBuild {
            recommended: promos.get(&format!("{mc}-recommended")).map(String::as_str)
                == Some(version),
            latest: promos.get(&format!("{mc}-latest")).map(String::as_str) == Some(version),
            version: version.to_string(),
            minecraft: Some(mc.to_string()),
        });
    }
    sort_newest_first(&mut out);
    out
}

/// Order builds newest first by their own numbers.
///
/// Not by document order, which was the first attempt and was wrong: Forge's
/// metadata lists 1.12.2 descending and then appends later re-releases, so
/// reversing it put `14.23.5.2860` -- the build this deployment runs -- at
/// position 354 of 355, behind a picker's cut. A version is a sequence of
/// numbers and is compared as one; anything unparseable sorts last rather than
/// disappearing.
fn sort_newest_first(builds: &mut [LoaderBuild]) {
    builds.sort_by(|a, b| {
        version_key(&b.minecraft.clone().unwrap_or_default())
            .cmp(&version_key(&a.minecraft.clone().unwrap_or_default()))
            .then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
    });
}

/// A version as the numbers in it, in order. `14.23.5.2860` sorts above
/// `14.23.5.2859` and below `14.23.5.2861`, which string comparison does not
/// manage past a digit boundary.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

/// NeoForge versions carry the Minecraft version in their own first two
/// segments: `21.1.x` is for 1.21.1. Derived rather than left blank, so the
/// same filter works for it as for Forge.
///
/// A `.0` minor is the bare release, not a patch: `21.0.x` is for 1.21, and
/// there is no such Minecraft version as 1.21.0. The picker filters on this
/// string by equality, so labelling those builds `1.21.0` emptied the loader
/// list for every major Minecraft release -- the exact versions a pack is most
/// likely to be started on.
fn parse_neoforge(body: &[u8]) -> Result<Vec<LoaderBuild>> {
    #[derive(Deserialize)]
    struct Versions {
        versions: Vec<String>,
    }
    let list: Versions = serde_json::from_slice(body).context("decoding neoforge versions")?;
    let mut out: Vec<LoaderBuild> = list
        .versions
        .into_iter()
        .map(|version| {
            let mut parts = version.split('.');
            let minecraft = match (parts.next(), parts.next()) {
                (Some(major), Some("0")) if major.chars().all(|c| c.is_ascii_digit()) => {
                    Some(format!("1.{major}"))
                }
                (Some(major), Some(minor)) if major.chars().all(|c| c.is_ascii_digit()) => {
                    Some(format!("1.{major}.{minor}"))
                }
                _ => None,
            };
            LoaderBuild {
                version,
                minecraft,
                recommended: false,
                latest: false,
            }
        })
        .collect();
    sort_newest_first(&mut out);
    mark_latest_per_minecraft(&mut out);
    Ok(out)
}

/// Fabric and Quilt publish one loader for every Minecraft version, so a build
/// carries none. `stable` is their word for recommended.
fn parse_fabric(body: &[u8]) -> Result<Vec<LoaderBuild>> {
    #[derive(Deserialize)]
    struct Entry {
        version: String,
        #[serde(default)]
        stable: bool,
    }
    let list: Vec<Entry> = serde_json::from_slice(body).context("decoding loader versions")?;
    Ok(list
        .into_iter()
        .map(|e| LoaderBuild {
            version: e.version,
            minecraft: None,
            recommended: e.stable,
            latest: false,
        })
        .collect())
}

/// Mark the newest build of each Minecraft version, for lists whose upstream
/// does not say. Expects newest-first order.
fn mark_latest_per_minecraft(builds: &mut [LoaderBuild]) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in builds.iter_mut() {
        if let Some(mc) = b.minecraft.clone()
            && seen.insert(mc)
        {
            b.latest = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<metadata><versioning><versions>
        <version>1.12.2-14.23.5.2859</version>
        <version>1.12.2-14.23.5.2860</version>
        <version>1.21.1-52.0.1</version>
    </versions></versioning></metadata>"#;

    // The reason the whole list is fetched rather than the promotions file:
    // this mirror's own pack is pinned to a build that is neither latest nor
    // recommended, and a picker built from promotions could not offer it.
    #[test]
    fn the_full_list_holds_builds_promotions_never_names() {
        let promos =
            br#"{"promos":{"1.12.2-latest":"14.23.5.2864","1.12.2-recommended":"14.23.5.2859"}}"#;
        let builds = parse_forge(XML, Some(promos));
        let versions: Vec<&str> = builds.iter().map(|b| b.version.as_str()).collect();
        assert!(
            versions.contains(&"14.23.5.2860"),
            "the pinned build is offered: {versions:?}"
        );
        let rec = builds.iter().find(|b| b.recommended).unwrap();
        assert_eq!(
            rec.version, "14.23.5.2859",
            "the marker comes from promotions"
        );
        assert!(
            builds.iter().all(|b| !b.latest),
            "1.12.2-latest names 2864, which is not in this list, so nothing is marked latest"
        );
    }

    // The shape Forge actually publishes: a descending run with later
    // re-releases appended. Reversing the document put the build this
    // deployment runs at the far end of the list, behind a picker's cut --
    // which is the whole reason the full list is fetched at all.
    #[test]
    fn builds_are_ordered_by_their_numbers_not_by_document_order() {
        let xml = r#"<metadata><versions>
            <version>1.12.2-14.23.5.2860</version>
            <version>1.12.2-14.23.5.2859</version>
            <version>1.12.2-14.23.5.2858</version>
            <version>1.12.2-14.23.0.2486</version>
            <version>1.12.2-14.23.5.2864</version>
        </versions></metadata>"#;
        let versions: Vec<String> = parse_forge(xml, None)
            .into_iter()
            .map(|b| b.version)
            .collect();
        assert_eq!(
            versions,
            vec![
                "14.23.5.2864",
                "14.23.5.2860",
                "14.23.5.2859",
                "14.23.5.2858",
                "14.23.0.2486"
            ],
            "newest first by number, and 2860 is near the top rather than last"
        );
    }

    #[test]
    fn forge_entries_carry_their_minecraft_version_and_run_newest_first() {
        let builds = parse_forge(XML, None);
        assert_eq!(builds[0].version, "52.0.1");
        assert_eq!(builds[0].minecraft.as_deref(), Some("1.21.1"));
        assert_eq!(builds.len(), 3);
    }

    // A missing or unreadable promotions file must not lose the list: the
    // markers are an enrichment, and a picker without them still offers every
    // build.
    #[test]
    fn promotions_are_optional() {
        assert_eq!(parse_forge(XML, None).len(), 3);
        assert_eq!(parse_forge(XML, Some(b"not json")).len(), 3);
    }

    // NeoForge names no Minecraft version, but its own first two segments are
    // it -- 21.1.x is for 1.21.1 -- so the same filter works for both loaders.
    #[test]
    fn neoforge_versions_say_which_minecraft_they_are_for() {
        let body = br#"{"isSnapshot":false,"versions":["21.1.9","21.1.10","20.4.1-beta"]}"#;
        let builds = parse_neoforge(body).unwrap();
        let versions: Vec<&str> = builds.iter().map(|b| b.version.as_str()).collect();
        assert_eq!(versions, vec!["21.1.10", "21.1.9", "20.4.1-beta"]);
        assert_eq!(builds[0].minecraft.as_deref(), Some("1.21.1"));
        assert_eq!(builds[2].minecraft.as_deref(), Some("1.20.4"));
        // the newest of each Minecraft version is marked, since upstream does not
        assert!(builds[0].latest, "21.1.10 is the newest 1.21.1 build");
        assert!(!builds[1].latest);
        assert!(
            builds[2].latest,
            "and 20.4.1-beta is the newest for its own"
        );
    }

    // A `.0` minor is the bare Minecraft release: 21.0.x is for 1.21, and there
    // is no 1.21.0 for the picker's equality filter to match -- so labelling it
    // that way emptied the loader list for every major release.
    #[test]
    fn a_zero_minor_is_the_bare_minecraft_release() {
        let body = br#"{"isSnapshot":false,"versions":["21.0.167","20.0.1","21.1.9"]}"#;
        let builds = parse_neoforge(body).unwrap();
        let mc = |v: &str| {
            builds
                .iter()
                .find(|b| b.version == v)
                .and_then(|b| b.minecraft.clone())
        };
        assert_eq!(mc("21.0.167").as_deref(), Some("1.21"));
        assert_eq!(mc("20.0.1").as_deref(), Some("1.20"));
        assert_eq!(
            mc("21.1.9").as_deref(),
            Some("1.21.1"),
            "and a real patch keeps its patch"
        );
    }

    // Fabric and Quilt publish one loader for every Minecraft version, so a
    // build carries none and `stable` is their word for recommended.
    #[test]
    fn fabric_builds_are_minecraft_independent() {
        let body = br#"[{"version":"0.16.9","stable":true},{"version":"0.16.10","stable":false}]"#;
        let builds = parse_fabric(body).unwrap();
        assert!(builds[0].minecraft.is_none());
        assert!(builds[0].recommended);
        assert!(!builds[1].recommended);
    }

    #[test]
    fn only_loaders_with_a_published_list_are_offered() {
        for known in ["forge", "NeoForge", " fabric ", "quilt"] {
            assert!(is_known(known), "{known}");
        }
        // a fork the registry knows about still has no upstream build list, and
        // the field stays free text rather than pretending otherwise
        for unknown in ["cleanroom", "lwjgl3ify", ""] {
            assert!(!is_known(unknown), "{unknown}");
        }
    }
}
