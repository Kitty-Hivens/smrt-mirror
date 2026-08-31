//! The lists the editor offers instead of a text box (#126).
//!
//! A pack's Minecraft version was typed by hand, so a typo travelled into the
//! manifest and announced itself as a launcher that would not start. The list
//! behind it is real and public, and this holds a copy of it.
//!
//! Held rather than proxied, for two reasons. Opening the editor must not depend
//! on somebody else's service being up -- and when it is down the honest answer
//! is the list as it was last known, not an empty picker, which is the same
//! shape the mod search already takes on a Modrinth outage. And the list changes
//! a few times a month, so asking per editor-open would be a request per
//! keystroke's worth of value.

use super::modrinth::{GameVersion, Modrinth};
use crate::storage::Storage;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use ts_rs::TS;

/// How stale a cached list may be before the next ask refreshes it. Minecraft
/// releases a few times a year and snapshots weekly; a day late is a list that
/// is still right about everything anyone is building against.
const MAX_AGE_SECS: u64 = 6 * 60 * 60;

/// Cache file name for the Minecraft list.
const MINECRAFT_LIST: &str = "minecraft-versions";

/// A cached list, with the answer to "how old is this?" rather than only the
/// list -- a stale list is worth serving and worth admitting to.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MinecraftVersions {
    pub versions: Vec<GameVersion>,
    /// When the mirror last heard this from upstream (RFC 3339), for whoever is
    /// reading the answer.
    pub fetched_at: String,
    /// The same instant as a number, which is what freshness is measured
    /// against. The mirror formats time and does not parse it anywhere, and a
    /// parser earned for one comparison is more surface than one integer.
    #[ts(type = "number")]
    pub fetched_unix: u64,
    /// This copy is past its freshness window: a refresh is in flight behind
    /// the answer, or upstream is not answering. Either way it is old news, and
    /// the panel says so rather than presenting it as current.
    #[serde(default)]
    pub stale: bool,
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn rfc3339(secs: u64) -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::from_unix_timestamp(secs as i64)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Is a cached list still worth serving without asking again?
///
/// A timestamp from the future is not fresh: a clock that moved backwards would
/// otherwise pin a cache forever, and re-fetching is cheap.
pub(super) fn is_fresh(fetched_unix: u64) -> bool {
    let now = unix_now();
    fetched_unix <= now && now - fetched_unix < MAX_AGE_SECS
}

/// Which lists are being refreshed right now.
static REFRESHING: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

/// The right to refresh `list`, or `None` because somebody already holds it.
///
/// A stale list is answered from the file and refreshed behind the answer, so
/// without this every ask arriving during a refresh starts another one: a
/// handful of editors opening together is a handful of fetches of the same
/// list, each writing the same file, and the loader lists are a few hundred
/// kilobytes apiece. The first ask through does the work; the rest are answered
/// from the copy they were going to be answered from anyway.
pub(super) fn refresh_ticket(list: &str) -> Option<RefreshTicket> {
    let mut held = REFRESHING.lock().ok()?;
    let set = held.get_or_insert_with(Default::default);
    set.insert(list.to_string())
        .then(|| RefreshTicket(list.to_string()))
}

/// Held for the duration of one refresh; releases on drop, so a refresh that
/// panics or returns early does not wedge the list forever.
pub(super) struct RefreshTicket(String);

#[cfg(test)]
mod refresh_tests {
    use super::refresh_ticket;

    // Ten editors opening at once must not be ten fetches of one list.
    #[test]
    fn one_refresh_at_a_time_per_list() {
        let first = refresh_ticket("a-list").expect("the first ask does the work");
        assert!(
            refresh_ticket("a-list").is_none(),
            "the rest are answered from the copy they already have"
        );
        assert!(
            refresh_ticket("another-list").is_some(),
            "a different list is not blocked by this one"
        );
        drop(first);
        assert!(
            refresh_ticket("a-list").is_some(),
            "and the next stale read refreshes again once this one is done"
        );
    }
}

impl Drop for RefreshTicket {
    fn drop(&mut self) {
        if let Ok(mut held) = REFRESHING.lock()
            && let Some(set) = held.as_mut()
        {
            set.remove(&self.0);
        }
    }
}

/// The Minecraft versions, from the cache when it is fresh and from upstream
/// when it is not.
///
/// An upstream failure is not an error here: the last known list is a better
/// answer than none, and the caller is told it is what it is.
pub async fn minecraft_versions(
    storage: &Arc<Storage>,
    modrinth: &Arc<Modrinth>,
) -> Result<MinecraftVersions> {
    let cached: Option<MinecraftVersions> =
        storage.load_meta_list(MINECRAFT_LIST).await.ok().flatten();
    if let Some(list) = cached {
        if is_fresh(list.fetched_unix) {
            return Ok(list);
        }
        // Stale, but there is something to serve. Nobody waits on somebody
        // else's service for a list that moves a few times a month: the old one
        // goes back now, and the refresh runs behind it. If upstream is down the
        // only casualty is that the next ask is also served from this file.
        if let Some(ticket) = refresh_ticket(MINECRAFT_LIST) {
            let (storage, modrinth) = (storage.clone(), modrinth.clone());
            tokio::spawn(async move {
                let _ticket = ticket;
                if let Err(e) = refresh(&storage, &modrinth).await {
                    tracing::warn!(error = %format!("{e:#}"), "refreshing the Minecraft version list failed");
                }
            });
        }
        // Said to be old, because it is. The flag used to mean "upstream was
        // unreachable"; measured against the window it means the same thing to
        // whoever reads it -- this is not current -- and it is true in the case
        // that actually happens, which is a refresh still in flight.
        return Ok(MinecraftVersions {
            stale: true,
            ..list
        });
    }

    // Nothing cached at all: this caller has to wait, because there is no older
    // answer to hand it. Every ask after this one is served from the file.
    refresh(storage, modrinth)
        .await
        .context("no cached Minecraft version list to fall back on")
}

/// Ask upstream and write what comes back, so the caller that triggered it and
/// every later one are answered from the same work.
async fn refresh(storage: &Arc<Storage>, modrinth: &Arc<Modrinth>) -> Result<MinecraftVersions> {
    let versions = modrinth.game_versions().await?;
    let now = unix_now();
    let fresh = MinecraftVersions {
        versions,
        fetched_at: rfc3339(now),
        fetched_unix: now,
        stale: false,
    };
    if let Err(e) = storage.save_meta_list(MINECRAFT_LIST, &fresh).await {
        tracing::warn!(error = %e, "caching the Minecraft version list failed");
    }
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_measured_from_when_it_was_fetched() {
        let now = unix_now();
        assert!(is_fresh(now - 60), "a minute old is fresh");
        assert!(is_fresh(now - MAX_AGE_SECS + 60), "just inside the window");
        assert!(!is_fresh(now - MAX_AGE_SECS - 60), "just outside it");
    }

    // A clock that moved backwards would otherwise pin the cache forever, and
    // asking again costs one request.
    #[test]
    fn a_timestamp_from_the_future_is_not_fresh() {
        assert!(!is_fresh(unix_now() + 3600));
    }

    // The formatted stamp is what the answer carries; it must be readable as
    // what it claims to be rather than a bare number in a string.
    #[test]
    fn the_reported_time_is_rfc3339() {
        let s = rfc3339(1_785_000_000);
        assert!(s.starts_with("2026-"), "got {s}");
        assert!(s.ends_with('Z'), "got {s}");
    }
}
