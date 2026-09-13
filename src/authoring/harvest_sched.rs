//! Background harvest coordinator: keeps the mod-identity registry fresh without
//! the operator ever running a harvest by hand. A `poke` is cheap and
//! non-blocking; pokes coalesce (a burst of cache writes, or a build that lands
//! many artifacts, settles into one harvest after a quiet window), and only one
//! harvest runs at a time since the worker is a single task. The manual
//! `/registry/harvest` endpoint stays as an immediate force-refresh.

use super::curseforge::CurseForge;
use super::harvest;
use super::harvest::HarvestReport;
use super::modrinth::Modrinth;
use crate::events::MirrorEvents;
use crate::registry::Registry;
use crate::registry::upsert::now_rfc3339;
use crate::storage::Storage;
use serde::Serialize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Quiet window after the last poke before a harvest fires. Long enough that a
/// burst (dragging in a stack of jars, a build writing many mod rows) collapses
/// into a single run instead of one harvest per write.
const DEBOUNCE: Duration = Duration::from_secs(45);

/// Observable state of the background harvester, served by the status endpoint so
/// an operator who kicked a forced harvest can watch it without holding the
/// request open (a full harvest can outlast a gateway timeout).
#[derive(Debug, Clone, Default, Serialize)]
pub struct HarvestStatus {
    /// A harvest is executing right now.
    pub running: bool,
    /// The report of the most recent completed harvest, if any.
    pub last_report: Option<HarvestReport>,
    /// The error of the most recent failed harvest, cleared on the next success.
    pub last_error: Option<String>,
    /// When the most recent harvest finished (RFC3339), success or failure.
    pub last_finished: Option<String>,
}

pub struct HarvestScheduler {
    storage: Arc<Storage>,
    modrinth: Arc<Modrinth>,
    /// Absent when no CurseForge key is configured. The harvest then runs with
    /// the Modrinth identity leg alone, which is what it did before this existed.
    curseforge: Option<Arc<CurseForge>>,
    registry: Arc<Registry>,
    /// Announced to when a run rewrites the index, so a registry view learns it
    /// went stale instead of finding out on its next refresh.
    events: Arc<MirrorEvents>,
    wake: Notify,
    /// Set by [`force`] so the worker skips the debounce and harvests at once.
    force_now: AtomicBool,
    /// A poke has been requested that no run has observed yet. Together with
    /// `status.running` this is the "unsettled" signal builds wait out, so a
    /// build never classifies against a registry a harvest is about to
    /// rewrite (or should have rewritten already).
    dirty: AtomicBool,
    status: Mutex<HarvestStatus>,
}

impl HarvestScheduler {
    pub fn new(
        storage: Arc<Storage>,
        modrinth: Arc<Modrinth>,
        curseforge: Option<Arc<CurseForge>>,
        registry: Arc<Registry>,
        events: Arc<MirrorEvents>,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            modrinth,
            curseforge,
            registry,
            events,
            wake: Notify::new(),
            force_now: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            status: Mutex::new(HarvestStatus::default()),
        })
    }

    /// Request a harvest soon. Non-blocking; safe to call from any handler after
    /// a change that the registry should reflect (a build, a cache write). A
    /// stored wake the worker drains -- repeated pokes before it fires are one.
    pub fn poke(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Request an immediate harvest, skipping the debounce window -- the operator's
    /// manual force-refresh. Non-blocking: the run happens on the worker and its
    /// result lands in [`status`], so the caller returns at once instead of holding
    /// the request open for the whole harvest.
    pub fn force(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.force_now.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// A snapshot of the harvester's state for the status endpoint.
    pub fn status(&self) -> HarvestStatus {
        self.status.lock().expect("harvest status mutex").clone()
    }

    /// True while a harvest is running or a poke is waiting to become one --
    /// the state a build should not classify under.
    pub fn is_unsettled(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
            || self.status.lock().expect("harvest status mutex").running
    }

    /// Wait until no harvest is running and no poke is pending, up to
    /// `max_wait`. Returns whether it settled (false = timed out; the caller
    /// proceeds against current state rather than starving). Half-second
    /// polling: one waiter, bounded wait, no missed-notify subtleties.
    pub async fn await_settled(&self, max_wait: Duration) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            if !self.is_unsettled() {
                return true;
            }
            if start.elapsed() >= max_wait {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Spawn the worker loop and request an initial harvest so the registry is
    /// built/refreshed shortly after startup. Call once, after the runtime is up.
    pub fn spawn(self: Arc<Self>) {
        self.poke(); // refresh on boot
        tokio::spawn(async move {
            loop {
                // wait for a poke, then settle: each further poke restarts the
                // window, so a whole burst coalesces into one run. A forced poke
                // (operator refresh) skips the settle entirely.
                self.wake.notified().await;
                if !self.force_now.swap(false, Ordering::SeqCst) {
                    loop {
                        tokio::select! {
                            _ = self.wake.notified() => {
                                if self.force_now.swap(false, Ordering::SeqCst) {
                                    break;
                                }
                                continue;
                            }
                            _ = tokio::time::sleep(DEBOUNCE) => break,
                        }
                    }
                }
                self.status.lock().expect("harvest status mutex").running = true;
                // this run observes everything requested up to now; a poke
                // arriving DURING it re-sets dirty and leaves a wake, so the
                // follow-up pass covers the genuinely-newer changes
                self.dirty.store(false, Ordering::SeqCst);
                // Run in a child task so a panic deep in harvest is isolated and
                // logged instead of killing this loop and silently stopping all
                // auto-harvests for the rest of the process.
                let (storage, modrinth, curseforge, registry) = (
                    self.storage.clone(),
                    self.modrinth.clone(),
                    self.curseforge.clone(),
                    self.registry.clone(),
                );
                let run = tokio::spawn(async move {
                    harvest::run_harvest(&storage, &modrinth, curseforge.as_deref(), registry).await
                });
                let result = run.await;
                let mut harvested = false;
                let mut st = self.status.lock().expect("harvest status mutex");
                st.running = false;
                st.last_finished = Some(now_rfc3339());
                match result {
                    Ok(Ok(rep)) => {
                        tracing::info!(
                            jars = rep.jars_scanned,
                            mods = rep.mods,
                            versions = rep.mod_versions,
                            builds = rep.builds,
                            inferred_requires = rep.inferred_requires,
                            inferred_optional = rep.inferred_optional,
                            modrinth_deps = rep.modrinth_deps,
                            declared_deps = rep.declared_deps,
                            sides = rep.sides_derived,
                            "auto-harvest complete"
                        );
                        st.last_error = None;
                        st.last_report = Some(rep);
                        harvested = true;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "auto-harvest failed");
                        st.last_error = Some(e.to_string());
                    }
                    Err(join) => {
                        tracing::error!(error = %join, "auto-harvest task crashed");
                        st.last_error = Some(format!("harvest task crashed: {join}"));
                    }
                }
                drop(st);
                if harvested {
                    self.events.registry("harvest");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> Arc<HarvestScheduler> {
        HarvestScheduler::new(
            Arc::new(Storage::new(std::env::temp_dir().join("smrt-sched-test"))),
            Arc::new(Modrinth::with_base("http://127.0.0.1:9").unwrap()),
            None,
            Arc::new(Registry::open_in_memory().unwrap()),
            Arc::new(MirrorEvents::default()),
        )
    }

    #[tokio::test]
    async fn settled_when_idle_unsettled_after_a_poke() {
        let s = bare();
        assert!(!s.is_unsettled(), "a fresh scheduler is settled");
        assert!(s.await_settled(Duration::from_millis(50)).await);

        s.poke();
        assert!(s.is_unsettled(), "a pending poke is unsettled");
        assert!(
            !s.await_settled(Duration::from_millis(200)).await,
            "with no worker draining it, the wait times out false"
        );

        // simulate the worker observing the poke: dirty clears, run starts
        s.dirty.store(false, Ordering::SeqCst);
        s.status.lock().unwrap().running = true;
        assert!(s.is_unsettled(), "a running harvest is unsettled");
        s.status.lock().unwrap().running = false;
        assert!(s.await_settled(Duration::from_millis(50)).await);
    }
}
