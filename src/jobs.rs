//! Build jobs with a live, tailable log. Authoring (enrichment passes +
//! manifest build) runs where the storage tree lives, so the panel triggers it
//! over HTTP and streams the log (SSE) instead of shelling out to `smrt-pack`.
//! A job is an in-memory log + status; `Notify` wakes SSE tailers on each new
//! line.

use crate::accounts::{Accounts, Identity};
use crate::authoring::{
    self, BootstrapArgs, HarvestScheduler, Modrinth, build_manifest, enrich_from_mcmod_info, gate,
    infer_requires_from_mcmod_info, make_pack_summary,
};
use crate::config::Config;
use crate::domain::{PackManifest, PackSummary, VersionChannel};
use crate::events::MirrorEvents;
use crate::registry::Registry;
use crate::storage::Storage;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Done,
    Failed,
}

/// The persisted form of a job: everything a status poll needs, written to
/// `jobs/<id>.json` at spawn and at finish so job ids survive a service
/// restart instead of turning into 404s. Dry-run results are not persisted --
/// a preview is a transient artifact of the session that requested it.
#[derive(Clone, Serialize, serde::Deserialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub kind: String,
    pub pack_id: String,
    pub status: Status,
    pub log: Vec<String>,
}

impl JobSnapshot {
    fn of(job: &Job) -> Self {
        let (log, status) = job.since(0);
        JobSnapshot {
            job_id: job.id.clone(),
            kind: job.kind.to_string(),
            pack_id: job.pack_id.clone(),
            status,
            log,
        }
    }
}

/// Best-effort persistence: a snapshot write failure is logged, never fatal --
/// the in-memory job keeps working and only restart survival degrades.
async fn persist(storage: &Storage, job: &Job) {
    if let Err(e) = storage.save_job_snapshot(&JobSnapshot::of(job)).await {
        tracing::warn!(job_id = %job.id, error = %e, "job snapshot write failed");
    }
}

/// What a dry-run build computes without publishing: the resolved manifest the
/// launcher would download, plus the summary card it would show. Stashed on the
/// job so the panel can render a launcher-faithful preview and diff it against
/// the currently-published manifest.
#[derive(Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DryRun {
    pub manifest: PackManifest,
    pub summary: PackSummary,
}

/// The service handles a build reaches for. They all live on `AppState` and
/// travel together, so they arrive together.
#[derive(Clone)]
pub struct BuildDeps {
    pub storage: Arc<Storage>,
    pub config: Arc<Config>,
    pub registry: Arc<Registry>,
    pub accounts: Arc<Accounts>,
    /// The mirror's one upstream client. Shared rather than built per build: it
    /// pools connections and remembers the project lookups it has already made,
    /// and both are thrown away by a client that lives for one build.
    pub modrinth: Arc<Modrinth>,
    /// The harvester to wait on (and poke afterwards), where one is running.
    pub harvest: Option<Arc<HarvestScheduler>>,
    /// Where a publish is announced, so a catalog stops being stale the moment
    /// the build lands rather than at whoever is looking next refresh.
    pub events: Arc<MirrorEvents>,
}

/// What a build was asked to do. One request rather than a row of positional
/// flags -- and the home of the override, which is a property of the request
/// and never of the pack.
pub struct BuildRequest {
    /// Compute + stash the manifest without publishing. A dry run judges the
    /// pack the same way and reports the verdict, but nothing it finds can stop
    /// it: there is nothing to stop.
    pub dry_run: bool,
    /// Canonical `pack_version`; `None` takes the next auto-numbered one.
    pub pack_version: Option<String>,
    pub channel: VersionChannel,
    pub changelog: Option<String>,
    /// The same notes per language tag, where the curator wrote more than one
    /// (see `PackManifest::changelog_i18n`).
    pub changelog_i18n: Option<std::collections::BTreeMap<String, String>>,
    /// Publish over a blocking pre-publish finding (#108). Never quiet: the job
    /// log says it, the audit trail records who asked, and the built manifest
    /// carries the findings it was published over.
    pub override_checks: bool,
    /// Republish over a version that already exists (#146). Never quiet: the
    /// job log says it, and it is refused without this.
    pub overwrite_version: bool,
    /// Build this commit's snapshot rather than the config on disk (#122).
    ///
    /// The live config is whatever is on screen at that instant, and with edits
    /// merging live (#115) that means pressing build while somebody is mid-rename
    /// builds half a word. A commit does not move while it is judged, so the
    /// pre-publish check (#108) judges the same thing that ships. `None` builds
    /// the live config -- the CLI takes a file and has no history to name.
    pub from_commit: Option<String>,
    /// Who asked for the build. `None` for a build with no session behind it
    /// (tests, and any future non-HTTP trigger) -- an override then records an
    /// unknown actor rather than borrowing someone's name.
    pub actor: Option<Identity>,
}

pub struct Job {
    pub id: String,
    pub kind: &'static str,
    pub pack_id: String,
    state: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    log: Vec<String>,
    status: Status,
    result: Option<DryRun>,
    blocked: Vec<String>,
}

impl Job {
    fn line(&self, text: impl Into<String>) {
        self.state.lock().unwrap().log.push(text.into());
        self.notify.notify_waiters();
    }

    fn finish(&self, status: Status) {
        self.state.lock().unwrap().status = status;
        self.notify.notify_waiters();
    }

    fn set_result(&self, result: DryRun) {
        self.state.lock().unwrap().result = Some(result);
    }

    /// The dry-run result, if this was a preview build that has produced one.
    pub fn result(&self) -> Option<DryRun> {
        self.state.lock().unwrap().result.clone()
    }

    fn set_blocked(&self, blocked: Vec<String>) {
        self.state.lock().unwrap().blocked = blocked;
    }

    /// What the pre-publish check would stop this build on. Kept beside the log
    /// rather than left for the panel to read back out of it: an offer to
    /// publish anyway must know it is answering the gate and not a resolve
    /// failure that no override can help.
    pub fn blocked(&self) -> Vec<String> {
        self.state.lock().unwrap().blocked.clone()
    }

    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }

    /// Log lines from index `from` onward, plus the current status. SSE tailers
    /// track how many lines they've sent and re-read from there, so no line is
    /// ever dropped regardless of `Notify` timing.
    pub fn since(&self, from: usize) -> (Vec<String>, Status) {
        let st = self.state.lock().unwrap();
        let tail = st.log.get(from..).map(|s| s.to_vec()).unwrap_or_default();
        (tail, st.status)
    }

    pub async fn wait(&self) {
        self.notify.notified().await;
    }
}

#[derive(Default)]
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    counter: AtomicU64,
    /// Per-pack lock so two real (publishing) builds of the same pack can't
    /// interleave save_manifest / set_latest / summary.
    pack_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl JobRegistry {
    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    fn pack_lock(&self, pack_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.pack_locks
            .lock()
            .unwrap()
            .entry(pack_id.to_string())
            .or_default()
            .clone()
    }

    fn create(&self, kind: &'static str, pack_id: String) -> Arc<Job> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let id = format!("{ms:013x}-{n:08}");
        let job = Arc::new(Job {
            id: id.clone(),
            kind,
            pack_id,
            state: Mutex::new(Inner {
                log: Vec::new(),
                status: Status::Running,
                result: None,
                blocked: Vec::new(),
            }),
            notify: Notify::new(),
        });
        let mut map = self.jobs.lock().unwrap();
        map.insert(id, job.clone());
        // Drop the oldest FINISHED job once the map is over the mark. Never a
        // running one: a client may still be tailing it, and its log is the only
        // copy until it finishes. So the bound is on finished jobs, not on the
        // map -- fifty builds running at once would hold fifty, and each is
        // holding a build's worth of work anyway. Ids are zero-padded
        // (ms + counter), so lexical min is the oldest.
        if map.len() > 50 {
            let victim = map
                .values()
                .filter(|j| j.status() != Status::Running)
                .map(|j| j.id.clone())
                .min();
            if let Some(victim) = victim {
                map.remove(&victim);
            }
        }
        job
    }

    /// Create a build job and run it on a background task. Returns immediately
    /// with the job handle so the caller can hand back a job id.
    pub fn spawn_build(&self, pack_id: String, deps: BuildDeps, req: BuildRequest) -> Arc<Job> {
        let BuildDeps {
            storage,
            config,
            registry,
            accounts,
            modrinth,
            harvest,
            events,
        } = deps;
        let dry_run = req.dry_run;
        let job = self.create(if dry_run { "preview" } else { "build" }, pack_id);
        let handle = job.clone();
        // Real builds of the same pack are serialized; a dry run never publishes
        // so it takes no lock.
        let lock = (!dry_run).then(|| self.pack_lock(&job.pack_id));
        tokio::spawn(async move {
            let _guard = match lock {
                Some(l) => Some(l.lock_owned().await),
                None => None,
            };
            // evidence on disk before any work: a crash mid-build leaves a
            // running snapshot for the startup sweep to mark interrupted
            persist(&storage, &handle).await;
            // Classify against a settled registry: an in-flight (or pending)
            // harvest is about to rewrite the very rows classification reads,
            // and building through it is how mods come out unclassified for
            // one build. Bounded so a busy harvester can never starve builds.
            if let Some(h) = &harvest
                && h.is_unsettled()
            {
                handle.line("waiting for the registry harvest to settle");
                if !h.await_settled(std::time::Duration::from_secs(300)).await {
                    handle
                        .line("harvest still busy after 5 minutes; building against current state");
                }
            }
            match run_build(
                &handle, &storage, &config, &registry, &accounts, &modrinth, req,
            )
            .await
            {
                Ok(()) => {
                    handle.finish(Status::Done);
                    // a published build added a build + its mods to harvest -- a
                    // dry run published nothing, so it needn't refresh
                    if !dry_run {
                        events.pack(&handle.pack_id, "published");
                        if let Some(h) = harvest {
                            h.poke();
                        }
                    }
                }
                Err(e) => {
                    handle.line(format!("failed: {e}"));
                    handle.finish(Status::Failed);
                }
            }
            persist(&storage, &handle).await;
        });
        job
    }

    /// Create a bootstrap job: stage an instance archive into the cache + static and
    /// write the starter authoring config, on a background task.
    pub fn spawn_bootstrap(
        &self,
        pack_id: String,
        args: BootstrapArgs,
        archive: Vec<u8>,
        storage: Arc<Storage>,
    ) -> Arc<Job> {
        let job = self.create("bootstrap", pack_id);
        let handle = job.clone();
        tokio::spawn(async move {
            persist(&storage, &handle).await;
            match run_bootstrap(&handle, args, archive, &storage).await {
                Ok(()) => handle.finish(Status::Done),
                Err(e) => {
                    handle.line(format!("failed: {e}"));
                    handle.finish(Status::Failed);
                }
            }
            persist(&storage, &handle).await;
        });
        job
    }
}

/// Load the pack's authoring inputs, run the build enrichment passes
/// transiently (config.json stays the source on disk), resolve sources, check
/// what the build would publish, and publish the manifest + summary + latest
/// pointer. Logs each step to the job.
#[allow(clippy::too_many_arguments)]
async fn run_build(
    job: &Job,
    storage: &Storage,
    config: &Config,
    registry: &Arc<Registry>,
    accounts: &Arc<Accounts>,
    modrinth: &Arc<Modrinth>,
    req: BuildRequest,
) -> Result<(), String> {
    let pack_id = job.pack_id.clone();
    job.line(format!("build {pack_id}: loading authoring inputs"));
    let mut cfg = match &req.from_commit {
        Some(id) => {
            job.line(format!("building from commit {id}"));
            storage
                .load_commit_config(&pack_id, id)
                .await
                .map_err(|e| format!("no such commit: {e}"))?
        }
        None => storage
            .load_pack_config(&pack_id)
            .await
            .map_err(|e| format!("no authoring config: {e}"))?,
    };
    job.line(format!(
        "config: {} mods, {} assets",
        cfg.mods.len(),
        cfg.assets.len()
    ));

    // Enrichment passes run on a transient copy of the config: fill display
    // metadata from each cache jar's mcmod.info, then infer the requires graph.
    //
    // Off the pool: both open and unzip every cache jar the pack names, which is
    // hundreds of blocking file reads on a large pack. Run inline they would
    // hold a runtime worker for the whole pass, and a mirror serving jars has
    // few of those.
    job.line("running enrichment passes (enrich-mcmod / infer-requires)");
    let root = storage.root().to_path_buf();
    cfg = tokio::task::spawn_blocking(move || {
        enrich_from_mcmod_info(&mut cfg, &root)
            .map_err(|e| format!("enrich-mcmod failed: {e:#}"))?;
        infer_requires_from_mcmod_info(&mut cfg, &root)
            .map_err(|e| format!("infer-requires failed: {e:#}"))?;
        Ok::<_, String>(cfg)
    })
    .await
    .map_err(|e| format!("enrichment task: {e}"))??;

    // The registry pass, in one hop off the pool: the side/policy classification
    // the required-ness seeds and side invariants ride on, and the dependency
    // resolve the pre-publish check judges. Both read the same rows of the same
    // enriched config, so they read them together.
    job.line("classifying mods and checking the pack against the registry");
    let (classifications, mut report) = {
        let reg = registry.clone();
        let cfg = cfg.clone();
        tokio::task::spawn_blocking(move || {
            reg.with_conn(|c| {
                Ok((
                    crate::authoring::resolve::classify_pack(c, &cfg)?,
                    crate::authoring::resolve::resolve_pack(c, &cfg)?,
                ))
            })
        })
        .await
        .map_err(|e| format!("registry task: {e}"))?
        .map_err(|e| format!("registry check failed: {e:#}"))?
    };

    // What each jar demands of the loader build (#164). Off the registry pass
    // because the answer is inside the jars, and a Modrinth pin's jar is not on
    // this disk: each artifact is read once, by range, and remembered. A pack
    // whose pin has fallen behind a mod's floor does not start, and until this
    // ran nothing said so before a player's crash log.
    job.line("checking the pinned loader build against what the mods declare");
    crate::authoring::loader_windows(&cfg, storage.root(), registry, modrinth)
        .await
        .apply(&mut report);

    // The gate (#108). Two findings mean the pack cannot start and stop a
    // publish; the rest are recorded onto the build. A dry run runs the same
    // check and reports the same verdict -- that is the point of previewing --
    // but publishes nothing, so nothing it finds can stop anything.
    let mut checks = gate::check(&report);
    for line in &checks.blocking {
        job.line(format!("blocking: {line}"));
    }
    for line in &checks.advisory {
        job.line(format!("noted: {line}"));
    }
    job.set_blocked(checks.blocking.clone());
    if !checks.blocking.is_empty() {
        if req.dry_run {
            job.line(format!(
                "{} problem(s) would stop a real build of this pack",
                checks.blocking.len()
            ));
        } else if !req.override_checks {
            return Err(format!(
                "{} problem(s) would keep this pack from starting -- fix them, or build again over the check",
                checks.blocking.len()
            ));
        } else {
            checks.overridden = true;
            job.line(format!(
                "publishing over {} blocking problem(s), at {}'s explicit request",
                checks.blocking.len(),
                actor_name(req.actor.as_ref()),
            ));
        }
    }

    job.line("resolving sources (Modrinth lookups + cache reads)");
    let built = build_manifest(
        &cfg,
        storage.root(),
        req.pack_version.as_deref(),
        req.channel,
        req.changelog,
        req.changelog_i18n.clone(),
        &config.mirror_base,
        &classifications,
        registry,
        modrinth,
    )
    .await
    .map_err(|e| format!("resolve failed: {e:#}"))?;
    let mut manifest = built.manifest;
    let published_over = checks.overridden.then_some(checks.blocking.len());
    // What was known about this build, on the build itself. A job log lives in
    // memory and its snapshot is evicted; the manifest is the artifact.
    manifest.checks = (!checks.is_empty()).then_some(checks);
    // The state this build came out of, named on the build, so "what shipped in
    // 0.1.24?" is answerable from the manifest alone rather than from whatever
    // the config happens to say today.
    manifest.built_from = req.from_commit.clone();
    let fell_back = built.resolved_from_registry;
    // A build that could not reach Modrinth and answered from the registry is
    // not the same event as one that reached it. It succeeds either way, but it
    // says which it was.
    if !fell_back.is_empty() {
        job.line(format!(
            "Modrinth unreachable for {} mod(s); resolved from the registry: {}",
            fell_back.len(),
            fell_back.join(", ")
        ));
    }
    let summary = make_pack_summary(&cfg, &manifest.pack_version);

    if req.dry_run {
        job.line(format!(
            "dry run: resolved {} ({} mods, {} assets) -- not publishing",
            manifest.pack_version,
            manifest.mods.len(),
            manifest.assets.len()
        ));
        job.set_result(DryRun { manifest, summary });
        return Ok(());
    }

    if req.overwrite_version {
        job.line(format!(
            "publishing over {} if it already exists, as asked -- anyone holding \
             that version keeps what they downloaded until they refetch",
            manifest.pack_version
        ));
    }
    job.line(format!(
        "writing manifest {} ({} mods, {} assets)",
        manifest.pack_version,
        manifest.mods.len(),
        manifest.assets.len()
    ));
    storage
        .save_manifest(&pack_id, &manifest, req.overwrite_version)
        .await
        .map_err(|e| e.to_string())?;
    storage
        .set_latest_manifest(&pack_id, &manifest.pack_version)
        .await
        .map_err(|e| e.to_string())?;
    storage
        .save_pack_summary(&summary)
        .await
        .map_err(|e| e.to_string())?;
    // Recorded once the pack is actually public: an override that ended in a
    // write failure published nothing, and the trail should not say it did.
    if let Some(n) = published_over {
        record_override(
            accounts,
            req.actor.as_ref(),
            &pack_id,
            &manifest.pack_version,
            n,
        )
        .await;
    }
    job.line(format!(
        "build complete: {pack_id} is now {}",
        manifest.pack_version
    ));
    Ok(())
}

/// Who asked, for the log line and the audit row. A build with no session
/// behind it says so rather than borrowing a name.
fn actor_name(actor: Option<&Identity>) -> &str {
    actor.map_or("an unattributed caller", |i| i.login.as_str())
}

/// Append the override to the system-wide audit trail. Best-effort, like every
/// other audit write: the pack is already published, and a lost trail entry
/// must not turn a finished build into a failed one.
async fn record_override(
    accounts: &Arc<Accounts>,
    actor: Option<&Identity>,
    pack_id: &str,
    pack_version: &str,
    blocking: usize,
) {
    let acc = accounts.clone();
    let (uid, login) = actor.map_or((0, "unknown".to_string()), |i| (i.uid, i.login.clone()));
    let target = pack_id.to_string();
    let detail = format!("{pack_version} published over {blocking} blocking check(s)");
    let res = tokio::task::spawn_blocking(move || {
        acc.record_audit(
            uid,
            &login,
            "build.override_checks",
            Some(&target),
            Some(&detail),
        )
    })
    .await;
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "override audit write failed"),
        Err(e) => tracing::warn!(error = %e, "override audit task failed"),
    }
}

async fn run_bootstrap(
    job: &Job,
    args: BootstrapArgs,
    archive: Vec<u8>,
    storage: &Storage,
) -> Result<(), String> {
    let pack_id = args.pack_id.clone();
    job.line(format!(
        "bootstrap {pack_id}: reading instance archive ({} bytes)",
        archive.len()
    ));
    let cfg = authoring::bootstrap(args, archive)
        .await
        .map_err(|e| e.to_string())?;
    job.line(format!(
        "discovered {} mods, {} assets; staged into cache + static",
        cfg.mods.len(),
        cfg.assets.len()
    ));
    // The handler refused a pack that already had a config; this is the same
    // question asked again with the lock held, which is what makes the answer
    // hold for the write that follows it. Unpacking the archive takes as long
    // as an archive takes, and a config can arrive in that time.
    let _guard = storage.lock_pack_config(&pack_id).await;
    if storage.load_pack_config(&pack_id).await.is_ok() {
        return Err(format!(
            "{pack_id} acquired a config while this archive was being read; nothing was written"
        ));
    }
    storage
        .save_pack_config(&pack_id, &cfg)
        .await
        .map_err(|e| e.to_string())?;
    job.line(format!(
        "wrote authoring config for {pack_id} -- ready to edit + build"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeclaredMod, LoaderSpec, PackConfig, PackTier, SourceDecl, Visibility};
    use sha1::{Digest, Sha1};
    use std::time::Duration;

    fn sha1_of(bytes: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn cfg_with_cache_mod(sha1: &str) -> PackConfig {
        PackConfig {
            pack_id: "Test".into(),
            display_name: "Test Pack".into(),
            tagline: "t".into(),
            minecraft_version: "1.12.2".into(),
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2860".into(),
            },
            java_major: 8,
            version: None,
            auth: None,
            tags: vec![],
            featured: false,
            mods: vec![DeclaredMod {
                filename: "Test.jar".into(),
                default_enabled: true,
                source: SourceDecl::SmrtCache { sha1: sha1.into() },
                display: None,
                slug: None,
                pulled: false,
            }],
            assets: vec![],
            pack_meta: Default::default(),
            owner: 211033194,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    fn test_config(storage_dir: std::path::PathBuf) -> Config {
        Config {
            bind_addr: "127.0.0.1:9000".parse().unwrap(),
            storage_dir,
            admin_token: None,
            cookie_secure: false,
            mirror_base: "https://test.example".into(),
            github_client_id: None,
            github_client_secret: None,
            admin_github_uids: Vec::new(),
            debug_token: None,
            debug_github_uids: Vec::new(),
        }
    }

    /// An ordinary build: publish, auto-numbered, no override.
    fn plain() -> BuildRequest {
        BuildRequest {
            dry_run: false,
            pack_version: None,
            channel: VersionChannel::Beta,
            changelog: None,
            changelog_i18n: None,
            override_checks: false,
            overwrite_version: false,
            from_commit: None,
            actor: None,
        }
    }

    fn accounts() -> Arc<Accounts> {
        Arc::new(Accounts::open_in_memory().unwrap())
    }

    /// Service handles for a build: an empty registry unless the test needs one
    /// with something to say, and no harvester (nothing to settle or poke).
    fn deps(
        storage: Arc<Storage>,
        config: Arc<Config>,
        registry: Option<Arc<Registry>>,
    ) -> BuildDeps {
        BuildDeps {
            storage,
            config,
            registry: registry.unwrap_or_else(|| Arc::new(Registry::open_in_memory().unwrap())),
            accounts: accounts(),
            modrinth: Arc::new(crate::authoring::Modrinth::new().unwrap()),
            harvest: None,
            events: Arc::new(MirrorEvents::default()),
        }
    }

    async fn await_finish(job: &Job) -> Status {
        for _ in 0..300 {
            let (_, status) = job.since(0);
            if status != Status::Running {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job did not finish within timeout");
    }

    /// A registry that knows the pack's one mod and that it hard-requires
    /// something the pack does not ship -- the shape of a pack that crashes on
    /// launch, which is what the gate exists for.
    fn registry_with_a_missing_hard_dep(sha1: &str) -> Arc<Registry> {
        use crate::registry::model::{RelKind, Source};
        use crate::registry::upsert;
        const NOW: &str = "2026-07-29T00:00:00Z";
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let id = upsert::upsert_mod_by_alias(c, &[("modid", "test")], NOW)?;
            upsert::upsert_mod_version(c, id, "1.0", &["forge"], sha1, 10, None, None, NOW)?;
            upsert::upsert_relation(
                c,
                id,
                None,
                "absentmod",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        Arc::new(r)
    }

    async fn pack_with_one_cached_mod() -> (tempfile::TempDir, Arc<Storage>, Arc<Config>, String) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::new(tmp.path().to_path_buf()));
        let bytes = b"jar";
        let sha1 = sha1_of(bytes);
        storage.save_cache_jar(&sha1, bytes).await.unwrap();
        storage
            .save_pack_config("Test", &cfg_with_cache_mod(&sha1))
            .await
            .unwrap();
        let config = Arc::new(test_config(tmp.path().to_path_buf()));
        (tmp, storage, config, sha1)
    }

    // The gate: a pack that cannot start does not reach the `latest` pointer the
    // launcher reads. Before this, the build wrote the manifest, moved the
    // pointer and rewrote the summary with nothing having asked (#108).
    #[tokio::test]
    async fn a_pack_that_cannot_start_is_not_published() {
        let (_tmp, storage, config, sha1) = pack_with_one_cached_mod().await;
        let jobs = JobRegistry::default();
        let job = jobs.spawn_build(
            "Test".into(),
            deps(
                storage.clone(),
                config,
                Some(registry_with_a_missing_hard_dep(&sha1)),
            ),
            plain(),
        );
        assert_eq!(await_finish(&job).await, Status::Failed);

        let (log, _) = job.since(0);
        let text = log.join("\n");
        assert!(
            text.contains("absentmod"),
            "the log names the unmet dependency: {text}"
        );
        assert!(
            job.blocked().iter().any(|b| b.contains("absentmod")),
            "and it is readable as a gate refusal, not just log prose: {:?}",
            job.blocked()
        );
        assert!(
            storage.load_latest_manifest("Test").await.is_err(),
            "nothing was published"
        );
    }

    // The override: a curator who knows better than the graph can publish, and
    // the mirror says so three times over -- in the log, in the audit trail, and
    // on the manifest the launcher downloads.
    #[tokio::test]
    async fn an_override_publishes_and_leaves_the_finding_on_the_build() {
        let (_tmp, storage, config, sha1) = pack_with_one_cached_mod().await;
        let accounts = accounts();
        let jobs = JobRegistry::default();
        let job = jobs.spawn_build(
            "Test".into(),
            BuildDeps {
                accounts: accounts.clone(),
                ..deps(
                    storage.clone(),
                    config,
                    Some(registry_with_a_missing_hard_dep(&sha1)),
                )
            },
            BuildRequest {
                override_checks: true,
                actor: Some(Identity {
                    uid: 7,
                    login: "curator".into(),
                    role: crate::accounts::Role::Admin,
                }),
                ..plain()
            },
        );
        assert_eq!(await_finish(&job).await, Status::Done);

        let published = storage.load_latest_manifest("Test").await.unwrap();
        let checks = published.checks.expect("the build carries what it knew");
        assert!(checks.overridden, "and that it was published over it");
        assert_eq!(checks.blocking.len(), 1);
        assert!(checks.blocking[0].contains("absentmod"));

        let rows = accounts.list_audit(10, None).unwrap();
        let row = rows
            .iter()
            .find(|r| r.action == "build.override_checks")
            .expect("the override reached the audit trail");
        assert_eq!(row.actor_login, "curator");
        assert_eq!(row.target.as_deref(), Some("Test"));
        assert!(
            row.detail.as_deref().unwrap_or_default().contains("1"),
            "the row says how much was overridden: {:?}",
            row.detail
        );
    }

    // A dry run judges the pack the same way and says what a real build would
    // refuse -- that is what previewing is for -- but it publishes nothing, so
    // it has nothing to refuse and finishes clean.
    #[tokio::test]
    async fn a_dry_run_reports_the_verdict_without_being_stopped_by_it() {
        let (_tmp, storage, config, sha1) = pack_with_one_cached_mod().await;
        let jobs = JobRegistry::default();
        let job = jobs.spawn_build(
            "Test".into(),
            deps(
                storage.clone(),
                config,
                Some(registry_with_a_missing_hard_dep(&sha1)),
            ),
            BuildRequest {
                dry_run: true,
                ..plain()
            },
        );
        assert_eq!(await_finish(&job).await, Status::Done);
        assert!(
            job.blocked().iter().any(|b| b.contains("absentmod")),
            "the preview still reports it: {:?}",
            job.blocked()
        );
        let result = job.result().expect("a preview manifest");
        assert!(
            result
                .manifest
                .checks
                .is_some_and(|c| !c.blocking.is_empty() && !c.overridden),
            "the preview carries the finding, unoverridden"
        );
    }

    #[tokio::test]
    async fn dry_run_computes_manifest_without_publishing() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::new(tmp.path().to_path_buf()));
        let bytes = b"fake jar bytes";
        let sha1 = sha1_of(bytes);
        storage.save_cache_jar(&sha1, bytes).await.unwrap();
        storage
            .save_pack_config("Test", &cfg_with_cache_mod(&sha1))
            .await
            .unwrap();
        let config = Arc::new(test_config(tmp.path().to_path_buf()));

        let registry = JobRegistry::default();
        let job = registry.spawn_build(
            "Test".into(),
            deps(storage.clone(), config, None),
            BuildRequest {
                dry_run: true,
                ..plain()
            },
        );
        assert_eq!(job.kind, "preview");
        assert_eq!(await_finish(&job).await, Status::Done);

        let result = job.result().expect("dry run stashes a result");
        assert_eq!(result.manifest.pack_id, "Test");
        assert_eq!(result.manifest.mods.len(), 1);
        assert_eq!(result.manifest.mods[0].filename, "Test.jar");
        assert_eq!(result.manifest.mods[0].sha1, sha1);
        assert_eq!(
            result.manifest.mods[0].size_bytes,
            b"fake jar bytes".len() as u64
        );
        assert_eq!(result.summary.display_name, "Test Pack");

        // The whole point of a dry run: nothing reaches the public surface.
        // (Its snapshot still persists, so the job id answers after a restart.)
        assert!(
            storage.load_latest_manifest("Test").await.is_err(),
            "dry run must not publish a latest manifest"
        );
        assert!(
            storage
                .list_manifest_versions("Test")
                .await
                .unwrap_or_default()
                .is_empty(),
            "dry run must not write a versioned manifest"
        );
    }

    #[tokio::test]
    async fn real_build_publishes_and_carries_no_dry_run_result() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::new(tmp.path().to_path_buf()));
        let bytes = b"jar";
        let sha1 = sha1_of(bytes);
        storage.save_cache_jar(&sha1, bytes).await.unwrap();
        storage
            .save_pack_config("Test", &cfg_with_cache_mod(&sha1))
            .await
            .unwrap();
        let config = Arc::new(test_config(tmp.path().to_path_buf()));

        let registry = JobRegistry::default();
        let job = registry.spawn_build(
            "Test".into(),
            deps(storage.clone(), config, None),
            BuildRequest {
                channel: VersionChannel::Release,
                ..plain()
            },
        );
        assert_eq!(job.kind, "build");
        assert_eq!(await_finish(&job).await, Status::Done);

        // the finished job leaves a snapshot a restarted service can answer
        // from; the write lands just after the status flip, so poll briefly
        let mut snap = None;
        for _ in 0..50 {
            snap = storage.load_job_snapshot(&job.id).await.unwrap();
            if snap.as_ref().is_some_and(|s| s.status == Status::Done) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let snap = snap.expect("a finished build persists its snapshot");
        assert_eq!(snap.status, Status::Done);
        assert_eq!(snap.pack_id, "Test");
        assert!(!snap.log.is_empty());

        assert!(
            job.result().is_none(),
            "a real build stashes no preview result"
        );
        let published = storage.load_latest_manifest("Test").await.unwrap();
        assert_eq!(published.mods.len(), 1);
        assert_eq!(published.mods[0].filename, "Test.jar");
    }

    // A publish changes what every catalog view is showing. Saying so is what
    // spares those views from asking on a timer.
    #[tokio::test]
    async fn a_publish_announces_itself_and_a_dry_run_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::new(tmp.path().to_path_buf()));
        let bytes = b"jar";
        let sha1 = sha1_of(bytes);
        storage.save_cache_jar(&sha1, bytes).await.unwrap();
        storage
            .save_pack_config("Test", &cfg_with_cache_mod(&sha1))
            .await
            .unwrap();
        let config = Arc::new(test_config(tmp.path().to_path_buf()));

        let events = Arc::new(MirrorEvents::default());
        let mut listening = events.subscribe(crate::accounts::Role::Member);
        let with_bus = |storage: Arc<Storage>, config: Arc<Config>| BuildDeps {
            events: events.clone(),
            ..deps(storage, config, None)
        };

        let registry = JobRegistry::default();
        let preview = registry.spawn_build(
            "Test".into(),
            with_bus(storage.clone(), config.clone()),
            BuildRequest {
                dry_run: true,
                ..plain()
            },
        );
        assert_eq!(await_finish(&preview).await, Status::Done);

        let job = registry.spawn_build("Test".into(), with_bus(storage.clone(), config), plain());
        assert_eq!(await_finish(&job).await, Status::Done);

        // the dry run published nothing, so the first thing heard is the build
        match listening.next().await.unwrap() {
            crate::events::MirrorEvent::Pack { pack_id, what } => {
                assert_eq!(pack_id, "Test");
                assert_eq!(what, "published");
            }
            other => panic!("expected a publish, got {other:?}"),
        }
    }
}
