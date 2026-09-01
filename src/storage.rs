use crate::domain::*;
use crate::http::ApiError;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
    /// Serializes the read-modify-write of removed.txt so concurrent takedowns
    /// don't lose each other's appends.
    removed_lock: Arc<tokio::sync::Mutex<()>>,
    /// One lock per pack id, serializing the read-modify-write of that pack's
    /// authoring config. See [`Storage::lock_pack_config`].
    config_locks: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Which builds came out of each commit, per pack. Derived from the
    /// manifests, which is a read of every retained manifest header -- fine
    /// once, wasteful on every page of a log and on every commit page. Dropped
    /// whenever a manifest is written, so it can only ever be behind by a build
    /// nobody has published yet.
    builds_by_commit: Arc<std::sync::Mutex<HashMap<String, Arc<BuildsByCommit>>>>,
}

/// Published versions per commit id, newest first within a commit.
type BuildsByCommit = HashMap<String, Vec<String>>;

impl Storage {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            removed_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            builds_by_commit: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Hold this for the whole load-check-write of one pack's authoring config.
    /// The config PUT reads the stored config, carries server-controlled fields
    /// and pulled dependencies over from it, then writes the result -- two of
    /// those interleaving drops whatever the first one merged in, which is the
    /// same lost update the `If-Match` check exists to stop, just from the
    /// other side (#52). Keyed by pack id, so packs never wait on each other.
    pub async fn lock_pack_config(&self, pack_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.config_locks.lock().unwrap();
            locks.entry(pack_id.to_string()).or_default().clone()
        };
        lock.lock_owned().await
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── Packs ──────────────────────────────────────────────────────────────

    pub async fn list_pack_summaries(&self) -> Result<Vec<PackSummary>, ApiError> {
        let packs_dir = self.root.join("packs");
        let mut out = Vec::new();
        // official packs: packs/<id>/summary.json
        read_summaries_in(&packs_dir, &mut out).await?;
        // community packs live a level deeper: packs/u/<uid>/<pack>/summary.json
        let u_dir = packs_dir.join("u");
        if let Ok(mut uids) = fs::read_dir(&u_dir).await {
            while let Some(uid) = uids.next_entry().await.map_err(io_err)? {
                if uid.file_type().await.map_err(io_err)?.is_dir() {
                    read_summaries_in(&uid.path(), &mut out).await?;
                }
            }
        }
        out.sort_by(|a, b| a.pack_id.cmp(&b.pack_id));
        Ok(out)
    }

    pub async fn load_pack_summary(&self, pack_id: &str) -> Result<PackSummary, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let path = self.root.join("packs").join(pack_id).join("summary.json");
        let bytes = fs::read(&path).await.map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    pub async fn load_latest_manifest(&self, pack_id: &str) -> Result<PackManifest, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let path = self
            .root
            .join("packs")
            .join(pack_id)
            .join("manifests")
            .join("latest");
        let bytes = fs::read(&path).await.map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    pub async fn load_manifest_version(
        &self,
        pack_id: &str,
        version: &str,
    ) -> Result<PackManifest, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        if !is_safe_version(version) {
            return Err(ApiError::BadRequest("invalid version slug".into()));
        }
        let path = self
            .root
            .join("packs")
            .join(pack_id)
            .join("manifests")
            .join(format!("{version}.json"));
        let bytes = fs::read(&path).await.map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    pub async fn list_manifest_versions(&self, pack_id: &str) -> Result<Vec<String>, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let path = self.root.join("packs").join(pack_id).join("manifests");
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&path).await.map_err(|_| ApiError::NotFound)?;
        while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip the `latest` symlink and anything not ending in .json.
            if name == "latest" || !name.ends_with(".json") {
                continue;
            }
            out.push(name.trim_end_matches(".json").to_string());
        }
        // Tuple comparison, not string sort: `.10` must land after `.2`
        // (the spec's ordering rule; string sort inverts it).
        out.sort_by(|a, b| compare_pack_versions(a, b));
        Ok(out)
    }

    /// Per-build metadata for every retained manifest of a pack, newest first
    /// (by `generated_at`, tuple version comparison as the tiebreak). Reads
    /// each manifest's header only -- the mod/asset arrays are counted, not
    /// materialized.
    pub async fn list_manifest_builds(
        &self,
        pack_id: &str,
    ) -> Result<Vec<ManifestBuildInfo>, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let dir = self.root.join("packs").join(pack_id).join("manifests");
        let mut out = Vec::new();
        let mut entries = fs::read_dir(&dir).await.map_err(|_| ApiError::NotFound)?;
        while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "latest" || !name.ends_with(".json") {
                continue;
            }
            let bytes = fs::read(entry.path()).await.map_err(io_err)?;
            // A manifest that no longer parses is skipped, not fatal: one
            // corrupt historical file must not take the whole listing down.
            let Ok(head) = serde_json::from_slice::<ManifestHead>(&bytes) else {
                continue;
            };
            out.push(build_info_from_head(head));
        }
        out.sort_by(|a, b| {
            b.date_published
                .cmp(&a.date_published)
                .then_with(|| compare_pack_versions(&b.version_number, &a.version_number))
        });
        Ok(out)
    }

    /// The version `manifests/latest` points at, resolved from the symlink
    /// target. `None` when the pack has no published build (or the link is
    /// unreadable) -- callers treat both the same way.
    pub async fn latest_manifest_version(&self, pack_id: &str) -> Result<Option<String>, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let latest = self
            .root
            .join("packs")
            .join(pack_id)
            .join("manifests")
            .join("latest");
        let Ok(target) = fs::read_link(&latest).await else {
            return Ok(None);
        };
        Ok(target
            .file_name()
            .map(|n| n.to_string_lossy().trim_end_matches(".json").to_string()))
    }

    /// Header metadata of the latest build only (one link resolve + one file
    /// read) -- the cheap form behind read-time summary enrichment.
    pub async fn latest_build_info(
        &self,
        pack_id: &str,
    ) -> Result<Option<ManifestBuildInfo>, ApiError> {
        let Some(version) = self.latest_manifest_version(pack_id).await? else {
            return Ok(None);
        };
        if !is_safe_version(&version) {
            return Ok(None);
        }
        let path = self
            .root
            .join("packs")
            .join(pack_id)
            .join("manifests")
            .join(format!("{version}.json"));
        let Ok(bytes) = fs::read(&path).await else {
            return Ok(None);
        };
        let Ok(head) = serde_json::from_slice::<ManifestHead>(&bytes) else {
            return Ok(None);
        };
        Ok(Some(build_info_from_head(head)))
    }

    /// Write a built manifest to `packs/<id>/manifests/<version>.json`. The
    /// authoring layer computes the manifest; persistence lives here so the
    /// on-disk layout has a single owner shared by the CLI and the panel.
    /// `overwrite` is the deliberate replacement of an already-published
    /// version, which is refused by default (#146).
    ///
    /// Auto-numbering never collides -- it takes the next counter past the
    /// highest published -- so this only fires when someone names a version by
    /// hand. Rewriting one is not a small thing: a launcher that already has
    /// that version and caches by version keeps the old content, the commit a
    /// manifest names as its origin stops being what shipped, and every diff
    /// that touches the version changes meaning retroactively.
    pub async fn save_manifest(
        &self,
        pack_id: &str,
        manifest: &PackManifest,
        overwrite: bool,
    ) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        if !is_safe_version(&manifest.pack_version) {
            return Err(ApiError::BadRequest("invalid pack version".into()));
        }
        let path = self
            .root
            .join("packs")
            .join(pack_id)
            .join("manifests")
            .join(format!("{}.json", manifest.pack_version));
        if !overwrite && fs::try_exists(&path).await.unwrap_or(false) {
            return Err(ApiError::Conflict(format!(
                "{} {} is already published; publishing over it would change what \
                 people already downloaded under that name",
                manifest.pack_id, manifest.pack_version
            )));
        }
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("manifest json encode: {e}")))?;
        atomic_write(&path, &bytes).await?;
        self.forget_builds_by_commit(pack_id);
        Ok(())
    }

    /// Which published versions came out of each commit of this pack, newest
    /// first within a commit.
    ///
    /// Answered from the manifests, and remembered: the alternative is reading
    /// every retained manifest header on every page of a log and again on every
    /// commit page. A pack whose manifests cannot be listed has no builds to
    /// name, which is the same answer as a pack that never published.
    pub async fn builds_by_commit(&self, pack_id: &str) -> Arc<BuildsByCommit> {
        if let Ok(cache) = self.builds_by_commit.lock()
            && let Some(known) = cache.get(pack_id)
        {
            return known.clone();
        }
        let mut built: BuildsByCommit = HashMap::new();
        if let Ok(builds) = self.list_manifest_builds(pack_id).await {
            for b in builds {
                if let Some(commit) = b.built_from {
                    built.entry(commit).or_default().push(b.version_number);
                }
            }
        }
        let built = Arc::new(built);
        if let Ok(mut cache) = self.builds_by_commit.lock() {
            cache.insert(pack_id.to_string(), built.clone());
        }
        built
    }

    /// Drop what was remembered about a pack's builds, because a manifest just
    /// moved under it.
    fn forget_builds_by_commit(&self, pack_id: &str) {
        if let Ok(mut cache) = self.builds_by_commit.lock() {
            cache.remove(pack_id);
        }
    }

    /// Point `manifests/latest` at `<version>.json` via an atomic symlink
    /// swap, so concurrent readers never observe a missing target. The
    /// public read path resolves `latest` by following the link.
    pub async fn set_latest_manifest(&self, pack_id: &str, version: &str) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        if !is_safe_version(version) {
            return Err(ApiError::BadRequest("invalid version slug".into()));
        }
        let manifests_dir = self.root.join("packs").join(pack_id).join("manifests");
        fs::create_dir_all(&manifests_dir).await.map_err(io_err)?;
        let target = format!("{version}.json");
        let latest = manifests_dir.join("latest");
        let latest_tmp = manifests_dir.join("latest.tmp");
        let _ = fs::remove_file(&latest_tmp).await;
        #[cfg(unix)]
        fs::symlink(&target, &latest_tmp).await.map_err(io_err)?;
        fs::rename(&latest_tmp, &latest).await.map_err(io_err)?;
        Ok(())
    }

    /// Write the pack's `summary.json` (the Browse-list / PackDetail card).
    pub async fn save_pack_summary(&self, summary: &PackSummary) -> Result<(), ApiError> {
        if !is_safe_id(&summary.pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let path = self
            .root
            .join("packs")
            .join(&summary.pack_id)
            .join("summary.json");
        let bytes = serde_json::to_vec_pretty(summary)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("summary json encode: {e}")))?;
        atomic_write(&path, &bytes).await
    }

    // ── Pack authoring inputs ────────────────────────────────────────────────
    //
    // The editable PackConfig the panel works from, kept on the mirror under
    // packs/<id>/authoring/ so the box that holds the mod cache is the single
    // home of both the authoring inputs and the built outputs.

    pub async fn save_pack_config(&self, pack_id: &str, cfg: &PackConfig) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let path = self.authoring_path(pack_id, "config.json");
        let bytes = serde_json::to_vec_pretty(cfg)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("pack config json encode: {e}")))?;
        atomic_write(&path, &bytes).await
    }

    pub async fn load_pack_config(&self, pack_id: &str) -> Result<PackConfig, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let bytes = fs::read(self.authoring_path(pack_id, "config.json"))
            .await
            .map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    // ── Pack history ─────────────────────────────────────────────────────────
    //
    // Commits live beside the config they checkpoint, under
    // packs/<id>/authoring/commits/. Metadata and snapshot are separate files
    // so walking the log reads kilobytes rather than megabytes, and `HEAD`
    // names the newest commit -- the chain runs backwards from there through
    // each commit's parent.

    fn commits_dir(&self, pack_id: &str) -> PathBuf {
        self.root
            .join("packs")
            .join(pack_id)
            .join("authoring")
            .join("commits")
    }

    /// Write a commit and move `HEAD` onto it.
    ///
    /// Order matters and is the whole reason this is one method: the snapshot
    /// lands first, then the metadata, then `HEAD`. A crash between any two
    /// leaves an unreferenced file rather than a `HEAD` pointing at a commit
    /// whose snapshot is missing, and an unreferenced file is invisible where a
    /// dangling `HEAD` would break the log for good.
    pub async fn save_commit(
        &self,
        pack_id: &str,
        commit: &crate::authoring::Commit,
        snapshot: &crate::authoring::CommitSnapshot,
    ) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        if !is_commit_id(&commit.id) {
            return Err(ApiError::BadRequest("invalid commit id".into()));
        }
        let dir = self.commits_dir(pack_id);
        fs::create_dir_all(&dir).await.map_err(io_err)?;

        let snapshot_bytes = serde_json::to_vec_pretty(snapshot)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("commit snapshot encode: {e}")))?;
        atomic_write(
            &dir.join(format!("{}.snapshot.json", commit.id)),
            &snapshot_bytes,
        )
        .await?;

        let meta_bytes = serde_json::to_vec_pretty(commit)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("commit encode: {e}")))?;
        atomic_write(&dir.join(format!("{}.json", commit.id)), &meta_bytes).await?;

        atomic_write(&dir.join("HEAD"), commit.id.as_bytes()).await?;
        // Everyone who saved into this commit is now named in it, so the
        // pending set starts again from empty.
        let _ = fs::remove_file(dir.join("pending.json")).await;
        Ok(())
    }

    /// The newest commit's id, or `None` on a pack that has never committed.
    pub async fn commit_head(&self, pack_id: &str) -> Option<String> {
        if !is_safe_id(pack_id) {
            return None;
        }
        let raw = fs::read_to_string(self.commits_dir(pack_id).join("HEAD"))
            .await
            .ok()?;
        let id = raw.trim().to_string();
        is_commit_id(&id).then_some(id)
    }

    pub async fn load_commit(
        &self,
        pack_id: &str,
        commit_id: &str,
    ) -> Result<crate::authoring::Commit, ApiError> {
        if !is_safe_id(pack_id) || !is_commit_id(commit_id) {
            return Err(ApiError::BadRequest("invalid commit id".into()));
        }
        let bytes = fs::read(self.commits_dir(pack_id).join(format!("{commit_id}.json")))
            .await
            .map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    pub async fn load_commit_config(
        &self,
        pack_id: &str,
        commit_id: &str,
    ) -> Result<PackConfig, ApiError> {
        if !is_safe_id(pack_id) || !is_commit_id(commit_id) {
            return Err(ApiError::BadRequest("invalid commit id".into()));
        }
        let path = self
            .commits_dir(pack_id)
            .join(format!("{commit_id}.snapshot.json"));
        let bytes = fs::read(path).await.map_err(|_| ApiError::NotFound)?;
        let snapshot: crate::authoring::CommitSnapshot =
            serde_json::from_slice(&bytes).map_err(json_err)?;
        Ok(snapshot.config)
    }

    /// The history newest first, walking parents from `HEAD` -- or from the
    /// commit after `after`, which is how a second page continues the first.
    ///
    /// The chain is the cursor: every commit names its parent, so continuing a
    /// walk needs no index and no offset, and a page boundary cannot drift when
    /// a commit lands while someone is reading. An `after` that will not load is
    /// treated as the end of the history rather than as the start of it --
    /// answering the whole log again would silently repeat the first page.
    ///
    /// Stops at a parent that will not load rather than failing the whole read:
    /// a truncated log still tells someone what the recent history was, where
    /// an error tells them nothing at all.
    pub async fn commit_log(
        &self,
        pack_id: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::authoring::Commit>, ApiError> {
        let mut out = Vec::new();
        let mut next = match after {
            Some(id) => match self.load_commit(pack_id, id).await {
                Ok(commit) => commit.parent,
                Err(_) => return Ok(out),
            },
            None => self.commit_head(pack_id).await,
        };
        while let Some(id) = next {
            if out.len() >= limit {
                break;
            }
            let Ok(commit) = self.load_commit(pack_id, &id).await else {
                break;
            };
            next = commit.parent.clone();
            out.push(commit);
        }
        Ok(out)
    }

    /// Note that someone saved, so the next commit can name them.
    ///
    /// A set rather than a log: the question a commit answers is who worked on
    /// it, and someone who saved forty times is one contributor.
    pub async fn note_pending_author(&self, pack_id: &str, who: &str) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) || who.is_empty() {
            return Ok(());
        }
        let dir = self.commits_dir(pack_id);
        fs::create_dir_all(&dir).await.map_err(io_err)?;
        let mut authors = self.pending_authors(pack_id).await;
        if authors.iter().any(|a| a == who) {
            return Ok(());
        }
        authors.push(who.to_string());
        let bytes = serde_json::to_vec(&authors)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("pending authors encode: {e}")))?;
        atomic_write(&dir.join("pending.json"), &bytes).await
    }

    pub async fn pending_authors(&self, pack_id: &str) -> Vec<String> {
        if !is_safe_id(pack_id) {
            return Vec::new();
        }
        fs::read(self.commits_dir(pack_id).join("pending.json"))
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Set a pack's publication state (the publish/unpublish toggle). Patches
    /// both the built `summary.json` -- what the public listing filters on, so
    /// the change takes effect without a rebuild -- and the authoring config, so
    /// a later rebuild keeps it. `NotFound` if the pack has neither file.
    pub async fn set_pack_visibility(
        &self,
        pack_id: &str,
        visibility: Visibility,
    ) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let mut found = false;
        if let Ok(mut summary) = self.load_pack_summary(pack_id).await {
            summary.visibility = visibility;
            self.save_pack_summary(&summary).await?;
            found = true;
        }
        let _guard = self.lock_pack_config(pack_id).await;
        if let Ok(mut cfg) = self.load_pack_config(pack_id).await {
            cfg.visibility = visibility;
            self.save_pack_config(pack_id, &cfg).await?;
            found = true;
        }
        if !found {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    /// Delete a pack and everything under it -- config, summary, manifests, and
    /// static assets. `NotFound` if the pack does not exist. The shared mod cache
    /// is content-addressed and left untouched; other packs keep their jars.
    pub async fn delete_pack(&self, pack_id: &str) -> Result<(), ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let dir = self.root.join("packs").join(pack_id);
        fs::remove_dir_all(&dir)
            .await
            .map_err(|_| ApiError::NotFound)
    }

    /// Clone an existing pack's authoring inputs under a new id: copy its
    /// `authoring/config.json` with `pack_id` rewritten (and the loader
    /// optionally overridden, for a loader-variant fork), then copy the whole
    /// per-pack `static/` tree. The mod cache is content-addressed and shared,
    /// so every `smrt_cache` / Modrinth source resolves under the new pack with
    /// no jar re-upload. Returns the new config.
    ///
    /// Refuses to overwrite a target that already has a config, so a variant
    /// can never clobber a working pack. Static is copied before the config is
    /// written -- a pack "exists" only once its config lands, so a mid-copy
    /// failure leaves no config.json and a retry runs clean.
    pub async fn duplicate_pack(
        &self,
        from: &str,
        to: &str,
        loader: Option<LoaderSpec>,
        owner: i64,
        fork_of: Option<String>,
    ) -> Result<PackConfig, ApiError> {
        if !is_safe_id(to) {
            return Err(ApiError::BadRequest("invalid target pack id".into()));
        }
        if from == to {
            return Err(ApiError::BadRequest(
                "source and target pack id are the same".into(),
            ));
        }
        // the existence check and the write of the clone are one step: two
        // duplicates racing onto the same target must not both pass the check
        let _guard = self.lock_pack_config(to).await;
        if fs::metadata(self.authoring_path(to, "config.json"))
            .await
            .is_ok()
        {
            return Err(ApiError::Conflict(format!(
                "pack {to:?} already has a config"
            )));
        }

        let mut cfg = self.load_pack_config(from).await?;
        cfg.pack_id = to.to_string();
        // the clone is a fresh draft owned by whoever made it; its tier follows
        // the target namespace, and fork_of is set only when this is a fork.
        cfg.owner = owner;
        cfg.tier = if to.starts_with("u/") {
            PackTier::Community
        } else {
            PackTier::Official
        };
        cfg.visibility = Visibility::Draft;
        cfg.fork_of = fork_of;
        if let Some(loader) = loader {
            cfg.loader = loader;
        }

        for rel in self.list_pack_static(from).await? {
            let bytes = fs::read(self.pack_static_path(from, &rel)?)
                .await
                .map_err(io_err)?;
            self.save_pack_static(to, &rel, &bytes).await?;
        }
        self.save_pack_config(to, &cfg).await?;
        Ok(cfg)
    }

    /// Pack ids that have authoring inputs (an `authoring/config.json`),
    /// including packs not yet built (so no summary.json). Sorted.
    pub async fn list_authoring_packs(&self) -> Result<Vec<String>, ApiError> {
        let packs_dir = self.root.join("packs");
        let mut out = Vec::new();
        // official ids sit at the top level; community ids are the full
        // `u/<uid>/<pack>` key, reconstructed from the nesting.
        read_authoring_ids_in(&packs_dir, "", &mut out).await?;
        let u_dir = packs_dir.join("u");
        if let Ok(mut uids) = fs::read_dir(&u_dir).await {
            while let Some(uid) = uids.next_entry().await.map_err(io_err)? {
                if uid.file_type().await.map_err(io_err)?.is_dir() {
                    let prefix = format!("u/{}/", uid.file_name().to_string_lossy());
                    read_authoring_ids_in(&uid.path(), &prefix, &mut out).await?;
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn authoring_path(&self, pack_id: &str, file: &str) -> PathBuf {
        self.root
            .join("packs")
            .join(pack_id)
            .join("authoring")
            .join(file)
    }

    /// Resolve a curated static asset path under the pack. Used by both the
    /// public GET and the admin PUT/DELETE endpoints; existence is not
    /// checked here so the same routine can produce destination paths for
    /// uploads.
    pub fn pack_static_path(&self, pack_id: &str, rel_path: &str) -> Result<PathBuf, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let safe = validate_rel_path(rel_path)?;
        Ok(self
            .root
            .join("packs")
            .join(pack_id)
            .join("static")
            .join(safe))
    }

    pub async fn save_pack_static(
        &self,
        pack_id: &str,
        rel_path: &str,
        bytes: &[u8],
    ) -> Result<(), ApiError> {
        let path = self.pack_static_path(pack_id, rel_path)?;
        atomic_write(&path, bytes).await
    }

    pub async fn delete_pack_static(&self, pack_id: &str, rel_path: &str) -> Result<(), ApiError> {
        let path = self.pack_static_path(pack_id, rel_path)?;
        fs::remove_file(&path)
            .await
            .map_err(|_| ApiError::NotFound)?;
        Ok(())
    }

    /// Every file under the pack's static area as relative paths (sorted).
    /// Backs the panel's Branding section (uploaded icons / banners / assets).
    pub async fn list_pack_static(&self, pack_id: &str) -> Result<Vec<String>, ApiError> {
        if !is_safe_id(pack_id) {
            return Err(ApiError::BadRequest("invalid pack id".into()));
        }
        let base = self.root.join("packs").join(pack_id).join("static");
        let mut out = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = match fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
                let path = entry.path();
                let ft = entry.file_type().await.map_err(io_err)?;
                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file()
                    && let Ok(rel) = path.strip_prefix(&base)
                {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    // ── Servers ────────────────────────────────────────────────────────────

    pub async fn list_servers(&self) -> Result<Vec<ServerEntry>, ApiError> {
        let dir = self.root.join("servers");
        let mut out = Vec::new();
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => return Ok(out),
        };
        while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".json") {
                continue;
            }
            if let Ok(bytes) = fs::read(entry.path()).await {
                match serde_json::from_slice::<ServerEntry>(&bytes) {
                    Ok(s) => out.push(s),
                    Err(e) => {
                        tracing::warn!(file = %name, error = %e, "skipping invalid server entry")
                    }
                }
            }
        }
        out.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        Ok(out)
    }

    pub async fn load_server(&self, server_id: &str) -> Result<ServerEntry, ApiError> {
        if !is_safe_id(server_id) {
            return Err(ApiError::BadRequest("invalid server id".into()));
        }
        let path = self.root.join("servers").join(format!("{server_id}.json"));
        let bytes = fs::read(&path).await.map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    // ── Featured ───────────────────────────────────────────────────────────

    pub async fn load_featured(&self) -> Result<Featured, ApiError> {
        let path = self.root.join("featured.json");
        let bytes = fs::read(&path).await.map_err(|_| ApiError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(json_err)
    }

    // ── Cache ──────────────────────────────────────────────────────────────

    pub fn cache_jar_path(&self, prefix: &str, sha1: &str) -> Result<PathBuf, ApiError> {
        if !is_hex(prefix) || prefix.len() != 2 {
            return Err(ApiError::BadRequest("invalid sha1 prefix".into()));
        }
        if !sha1.starts_with(prefix) {
            return Err(ApiError::BadRequest("prefix does not match sha1".into()));
        }
        cache_jar_path_in(&self.root, sha1)
            .ok_or_else(|| ApiError::BadRequest("invalid sha1".into()))
    }

    /// What a jar's icon lookup found without opening the jar.
    ///
    /// Extraction means reading a whole jar (megabytes) and walking a zip to
    /// pull out a few kilobytes of image, and the answer never changes: a jar is
    /// addressed by its own hash, so its icon is a property of that hash. Kept
    /// beside the cache under `icons/`, in the same two-character shards.
    ///
    /// A jar that carries no icon is remembered too. Without that, every request
    /// for the mods that have none -- and most of them have none -- would go on
    /// reading and unzipping the jar to rediscover it.
    pub async fn read_cached_icon(&self, sha1: &str) -> IconRead {
        let Some(dir) = self.icon_dir(sha1) else {
            return IconRead::Unknown;
        };
        for (ext, content_type) in ICON_KINDS {
            if let Ok(bytes) = fs::read(dir.join(format!("{sha1}.{ext}"))).await {
                return IconRead::Image {
                    bytes,
                    content_type,
                };
            }
        }
        if fs::metadata(dir.join(format!("{sha1}.{NO_ICON}")))
            .await
            .is_ok()
        {
            return IconRead::Absent;
        }
        IconRead::Unknown
    }

    /// Remember what a jar's icon turned out to be -- an image, or nothing.
    /// Best-effort: a cache that cannot be written costs the next request an
    /// extraction, which is what every request used to cost.
    pub async fn store_icon(&self, sha1: &str, icon: Option<(&[u8], &str)>) {
        let Some(dir) = self.icon_dir(sha1) else {
            return;
        };
        if fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        let (name, bytes): (String, &[u8]) = match icon {
            Some((bytes, content_type)) => {
                let ext = ICON_KINDS
                    .iter()
                    .find(|(_, ct)| *ct == content_type)
                    .map(|(ext, _)| *ext)
                    .unwrap_or("png");
                (format!("{sha1}.{ext}"), bytes)
            }
            None => (format!("{sha1}.{NO_ICON}"), &[]),
        };
        if let Err(e) = fs::write(dir.join(name), bytes).await {
            tracing::warn!(sha1, error = %e, "could not cache an extracted icon");
        }
    }

    /// A Modrinth project's icon as this mirror holds it, if it is still fresh.
    ///
    /// The mirror does not rehost Modrinth's jars -- that is the archival policy
    /// -- but a project icon is a few kilobytes of picture, not the mod, and
    /// holding it is what keeps a viewer's browser from fetching it off someone
    /// else's CDN. The same argument the avatar proxy already makes.
    ///
    /// Unlike a jar's icon this one can change upstream, so a copy older than
    /// [`PROJECT_ICON_MAX_AGE`] reads as absent and is fetched again.
    pub async fn read_project_icon(&self, project: &str) -> Option<(Vec<u8>, &'static str)> {
        let dir = self.project_icon_dir(project)?;
        for (ext, content_type) in ICON_KINDS {
            let path = dir.join(format!("{project}.{ext}"));
            let Ok(meta) = fs::metadata(&path).await else {
                continue;
            };
            let fresh = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < PROJECT_ICON_MAX_AGE);
            if !fresh {
                continue;
            }
            if let Ok(bytes) = fs::read(&path).await {
                return Some((bytes, content_type));
            }
        }
        None
    }

    /// Keep a project's icon. Best-effort, like the jar icons: a copy that
    /// cannot be written costs the next viewer one fetch.
    pub async fn store_project_icon(&self, project: &str, bytes: &[u8], content_type: &str) {
        let Some(ext) = ICON_KINDS
            .iter()
            .find(|(_, ct)| *ct == content_type)
            .map(|(ext, _)| *ext)
        else {
            return; // a kind we do not serve back; see ICON_KINDS
        };
        let Some(dir) = self.project_icon_dir(project) else {
            return;
        };
        if fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        if let Err(e) = fs::write(dir.join(format!("{project}.{ext}")), bytes).await {
            tracing::warn!(project, error = %e, "could not keep a project icon");
        }
    }

    /// A GitHub avatar the mirror already holds, while it is still fresh.
    ///
    /// The proxy exists so a page never hands a viewer's address to GitHub. It
    /// held nothing, though, so the mirror itself asked GitHub once per image
    /// per page load -- a byline on every comment, a row in the user list. The
    /// same argument as the project icons, and the same window: a picture that
    /// changes rarely, re-asked for daily.
    pub async fn read_avatar(&self, uid: i64) -> Option<(Vec<u8>, &'static str)> {
        let dir = self.root.join("icons").join("avatars");
        for (ext, content_type) in ICON_KINDS {
            let path = dir.join(format!("{uid}.{ext}"));
            let Ok(meta) = fs::metadata(&path).await else {
                continue;
            };
            let fresh = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < AVATAR_MAX_AGE);
            if !fresh {
                continue;
            }
            if let Ok(bytes) = fs::read(&path).await {
                return Some((bytes, content_type));
            }
        }
        None
    }

    /// Keep an avatar. Best-effort: a copy that cannot be written costs the
    /// next viewer one fetch.
    pub async fn store_avatar(&self, uid: i64, bytes: &[u8], content_type: &str) {
        let Some(ext) = ICON_KINDS
            .iter()
            .find(|(_, ct)| *ct == content_type)
            .map(|(ext, _)| *ext)
        else {
            return; // a kind we do not serve back; see ICON_KINDS
        };
        let dir = self.root.join("icons").join("avatars");
        if fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        if let Err(e) = fs::write(dir.join(format!("{uid}.{ext}")), bytes).await {
            tracing::warn!(uid, error = %e, "could not keep an avatar");
        }
    }

    /// Project ids are Modrinth's own base62 handles; a slug may also arrive.
    /// Anything that could leave the directory, or is not a plausible handle at
    /// all, gets no path -- this becomes a filename.
    fn project_icon_dir(&self, project: &str) -> Option<PathBuf> {
        let sane = !project.is_empty()
            && project.len() <= 64
            && project
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        sane.then(|| self.root.join("icons").join("modrinth"))
    }

    /// Drop whatever was remembered about a jar's icon. A taken-down jar must
    /// not keep serving the picture that came out of it.
    pub async fn forget_icon(&self, sha1: &str) {
        let Some(dir) = self.icon_dir(sha1) else {
            return;
        };
        for (ext, _) in ICON_KINDS {
            let _ = fs::remove_file(dir.join(format!("{sha1}.{ext}"))).await;
        }
        let _ = fs::remove_file(dir.join(format!("{sha1}.{NO_ICON}"))).await;
    }

    fn icon_dir(&self, sha1: &str) -> Option<PathBuf> {
        (sha1.len() == 40 && is_hex(sha1)).then(|| self.root.join("icons").join(sha1_shard(sha1)))
    }

    /// How big the cached jar is, or `None` when the mirror does not hold it.
    ///
    /// A jar's path is its own hash, so this is one `stat` rather than a look
    /// through a list. The list is how these questions used to be answered,
    /// which meant opening every directory in the cache to learn one thing about
    /// one jar -- a cost that grew with the mirror while the question stayed the
    /// same size.
    pub async fn cache_jar_size(&self, sha1: &str) -> Option<u64> {
        let path = cache_jar_path_in(&self.root, sha1)?;
        fs::metadata(&path).await.ok().map(|m| m.len())
    }

    /// Does the mirror hold this jar?
    pub async fn has_cache_jar(&self, sha1: &str) -> bool {
        self.cache_jar_size(sha1).await.is_some()
    }

    /// Which of `shas` the mirror holds.
    ///
    /// For a caller that already knows which artifacts it is asking about -- the
    /// files of one mod, the rows of one page -- this costs one `stat` per
    /// artifact asked about instead of a walk of everything the mirror has. Not
    /// a substitute for [`list_cache_inventory`](Self::list_cache_inventory),
    /// which answers "what is in there" rather than "is this in there".
    pub async fn cached_among<I, S>(&self, shas: I) -> HashSet<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut held = HashSet::new();
        for sha1 in shas {
            let sha1 = sha1.as_ref();
            if !held.contains(sha1) && self.has_cache_jar(sha1).await {
                held.insert(sha1.to_string());
            }
        }
        held
    }

    pub async fn list_cache_inventory(&self) -> Result<Vec<CacheInventoryEntry>, ApiError> {
        self.cache_inventory_after(None, None).await
    }

    /// The cache in sha1 order, starting after `after` and stopping at `take`.
    ///
    /// A jar lives under the first two characters of its own hash, so sha1 order
    /// is directory order: a page that starts mid-hash opens the directories
    /// from that point on and stops as soon as it is full, instead of walking
    /// the whole cache and throwing away everything before the cursor. On a
    /// mirror holding thousands of jars, that is the difference between a page
    /// costing a page and a page costing the cache.
    pub async fn cache_inventory_after(
        &self,
        after: Option<&str>,
        take: Option<usize>,
    ) -> Result<Vec<CacheInventoryEntry>, ApiError> {
        let cache_dir = self.root.join("cache");
        let mut out = Vec::new();
        let mut prefix_dirs = match fs::read_dir(&cache_dir).await {
            Ok(e) => e,
            Err(_) => return Ok(out),
        };
        let mut prefixes = Vec::new();
        while let Some(prefix_entry) = prefix_dirs.next_entry().await.map_err(io_err)? {
            if prefix_entry.file_type().await.map_err(io_err)?.is_dir() {
                prefixes.push(prefix_entry);
            }
        }
        prefixes.sort_by_key(|e| e.file_name());
        for prefix_entry in prefixes {
            // whole directories that sort before the cursor hold nothing the
            // caller has not already been given
            if let Some(cursor) = after {
                let name = prefix_entry.file_name();
                if name.to_string_lossy().as_ref() < &cursor[..cursor.len().min(2)] {
                    continue;
                }
            }
            let mut jars = fs::read_dir(prefix_entry.path()).await.map_err(io_err)?;
            let mut here = Vec::new();
            while let Some(jar) = jars.next_entry().await.map_err(io_err)? {
                let name = jar.file_name();
                let name = name.to_string_lossy();
                if let Some(sha1) = name.strip_suffix(".jar")
                    && is_hex(sha1)
                    && sha1.len() == 40
                    && after.is_none_or(|cursor| sha1 > cursor)
                {
                    let meta = jar.metadata().await.map_err(io_err)?;
                    here.push(CacheInventoryEntry {
                        sha1: sha1.to_string(),
                        size_bytes: meta.len(),
                    });
                }
            }
            here.sort_by(|a, b| a.sha1.cmp(&b.sha1));
            out.append(&mut here);
            if take.is_some_and(|n| out.len() >= n) {
                out.truncate(take.unwrap_or(out.len()));
                return Ok(out);
            }
        }
        Ok(out)
    }

    // ── Admin writes ───────────────────────────────────────────────────────

    pub async fn save_server(&self, entry: &ServerEntry) -> Result<(), ApiError> {
        if !is_safe_id(&entry.server_id) {
            return Err(ApiError::BadRequest("invalid server id".into()));
        }
        let dir = self.root.join("servers");
        fs::create_dir_all(&dir).await.map_err(io_err)?;
        let path = dir.join(format!("{}.json", entry.server_id));
        let bytes = serde_json::to_vec_pretty(entry)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("server json encode: {e}")))?;
        atomic_write(&path, &bytes).await
    }

    pub async fn delete_server(&self, server_id: &str) -> Result<(), ApiError> {
        if !is_safe_id(server_id) {
            return Err(ApiError::BadRequest("invalid server id".into()));
        }
        let path = self
            .root
            .join("servers")
            .join(format!("{}.json", server_id));
        fs::remove_file(&path)
            .await
            .map_err(|_| ApiError::NotFound)?;
        Ok(())
    }

    pub async fn save_cache_jar(&self, sha1: &str, bytes: &[u8]) -> Result<(), ApiError> {
        if !is_hex(sha1) || sha1.len() != 40 {
            return Err(ApiError::BadRequest("invalid sha1".into()));
        }
        // Content-addressed: verify body hashes to the claimed sha1 so a
        // mis-uploaded jar fails loudly rather than corrupting the cache.
        let actual = sha1_hex(bytes);
        if actual != sha1 {
            return Err(ApiError::BadRequest(format!(
                "sha1 mismatch: url claims {sha1} but body hashes to {actual}"
            )));
        }
        // removed.txt blocks re-ingestion of takedown'd jars; honor it on
        // upload too so a takedown survives a retry.
        if self.is_sha1_removed(sha1).await? {
            return Err(ApiError::BadRequest(format!(
                "sha1 {sha1} is on the removed-list and cannot be re-uploaded"
            )));
        }
        let path = cache_jar_path_in(&self.root, sha1)
            .ok_or_else(|| ApiError::BadRequest("invalid sha1".into()))?;
        if fs::metadata(&path).await.is_ok() {
            return Ok(());
        }
        atomic_write(&path, bytes).await
    }

    /// Free a cached jar's bytes. Reversible: the sha1 is not blocked, so the
    /// same jar can be re-added later. A deliberate policy block is `takedown`,
    /// a separate act -- deleting to reclaim space must not tombstone a jar (#14).
    pub async fn delete_cache_jar(&self, sha1: &str) -> Result<(), ApiError> {
        if !is_hex(sha1) || sha1.len() != 40 {
            return Err(ApiError::BadRequest("invalid sha1".into()));
        }
        let path = cache_jar_path_in(&self.root, sha1)
            .ok_or_else(|| ApiError::BadRequest("invalid sha1".into()))?;
        fs::remove_file(&path)
            .await
            .map_err(|_| ApiError::NotFound)?;
        Ok(())
    }

    /// Block a jar: drop any cached copy and add its sha1 to the removed list so
    /// it can neither be served nor re-ingested. The deliberate act for copyright
    /// or policy, distinct from `delete_cache_jar` which only frees bytes (#14).
    pub async fn takedown(&self, sha1: &str) -> Result<(), ApiError> {
        if !is_hex(sha1) || sha1.len() != 40 {
            return Err(ApiError::BadRequest("invalid sha1".into()));
        }
        if let Some(path) = cache_jar_path_in(&self.root, sha1) {
            let _ = fs::remove_file(&path).await; // best-effort: may not be cached
        }
        // the icon came out of those bytes, so it goes with them
        self.forget_icon(sha1).await;
        self.record_removed(sha1).await
    }

    /// Lift a takedown: remove the sha1 from the removed list so the jar may be
    /// cached and served again. The bytes are not restored -- re-add to recache.
    pub async fn restore(&self, sha1: &str) -> Result<(), ApiError> {
        if !is_hex(sha1) || sha1.len() != 40 {
            return Err(ApiError::BadRequest("invalid sha1".into()));
        }
        let _guard = self.removed_lock.lock().await;
        let path = self.root.join("removed.txt");
        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let mut out: String = content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && *l != sha1)
            .collect::<Vec<_>>()
            .join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        atomic_write(&path, out.as_bytes()).await
    }

    /// Stage a member upload pending moderation, content-addressed by sha1 under
    /// `uploads/`. Not the shared cache -- an operator promotes it on approval.
    pub async fn stage_upload(&self, sha1: &str, bytes: &[u8]) -> Result<(), ApiError> {
        if !is_hex(sha1) || sha1.len() != 40 {
            return Err(ApiError::BadRequest("invalid sha1".into()));
        }
        let actual = sha1_hex(bytes);
        if actual != sha1 {
            return Err(ApiError::BadRequest(format!(
                "sha1 mismatch: {sha1} vs {actual}"
            )));
        }
        atomic_write(&self.staged_upload_path(sha1), bytes).await
    }

    /// Promote a staged upload into the shared cache (moderation approved), then
    /// remove the staging copy. `NotFound` if nothing is staged.
    pub async fn promote_upload(&self, sha1: &str) -> Result<(), ApiError> {
        let staged = self.staged_upload_path(sha1);
        let bytes = fs::read(&staged).await.map_err(|_| ApiError::NotFound)?;
        self.save_cache_jar(sha1, &bytes).await?;
        let _ = fs::remove_file(&staged).await;
        Ok(())
    }

    /// Drop a staged upload (moderation rejected). Idempotent.
    pub async fn discard_upload(&self, sha1: &str) -> Result<(), ApiError> {
        let _ = fs::remove_file(self.staged_upload_path(sha1)).await;
        Ok(())
    }

    fn staged_upload_path(&self, sha1: &str) -> PathBuf {
        self.root.join("uploads").join(format!("{sha1}.jar"))
    }

    pub async fn save_featured(&self, featured: &Featured) -> Result<(), ApiError> {
        let path = self.root.join("featured.json");
        let bytes = serde_json::to_vec_pretty(featured)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("featured json encode: {e}")))?;
        atomic_write(&path, &bytes).await
    }

    /// A cached upstream list (#126), by name. `None` when the mirror has never
    /// fetched this one, which is a state and not a failure -- a fresh
    /// deployment has nothing to fall back on and says so by asking upstream.
    ///
    /// A corrupt cache reads as a miss: the list is re-fetchable, and refusing
    /// to open the editor over a bad file would be the wrong trade.
    pub async fn load_meta_list<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, ApiError> {
        let path = self.root.join("meta").join(format!("{name}.json"));
        let Ok(bytes) = fs::read(&path).await else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    pub async fn save_meta_list<T: serde::Serialize>(
        &self,
        name: &str,
        list: &T,
    ) -> Result<(), ApiError> {
        let dir = self.root.join("meta");
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("meta dir: {e}")))?;
        let bytes = serde_json::to_vec(list)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("{name} json encode: {e}")))?;
        atomic_write(&dir.join(format!("{name}.json")), &bytes).await
    }

    pub async fn is_sha1_removed(&self, sha1: &str) -> Result<bool, ApiError> {
        let path = self.root.join("removed.txt");
        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return Ok(false),
        };
        Ok(content.lines().any(|line| line.trim() == sha1))
    }

    async fn record_removed(&self, sha1: &str) -> Result<(), ApiError> {
        let _guard = self.removed_lock.lock().await;
        let path = self.root.join("removed.txt");
        let mut content = fs::read_to_string(&path).await.unwrap_or_default();
        if content.lines().any(|line| line.trim() == sha1) {
            return Ok(());
        }
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(sha1);
        content.push('\n');
        atomic_write(&path, content.as_bytes()).await
    }

    /// The takedown list: sha1s blocked from (re-)ingestion. Surfaced in the
    /// panel's Cache tab so an operator can see what has been removed.
    pub async fn list_removed(&self) -> Result<Vec<String>, ApiError> {
        let path = self.root.join("removed.txt");
        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(content
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}

/// Per-process temp-file sequence so concurrent writers to the same target use
/// distinct temp files instead of colliding on one shared `<file>.tmp`.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let parent = path.parent().ok_or_else(|| {
        ApiError::Internal(anyhow::anyhow!("path {} has no parent", path.display()))
    })?;
    fs::create_dir_all(parent).await.map_err(io_err)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.to_path_buf();
    let mut tmp_name = tmp
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    tmp_name.push(format!(".tmp.{}.{seq}", std::process::id()));
    tmp.set_file_name(tmp_name);

    // Write + fsync the temp, then rename. fsync before rename so a crash can't
    // leave a zero-length file at the target; clean up the temp on any error.
    let result = async {
        let mut f = fs::File::create(&tmp).await.map_err(io_err)?;
        f.write_all(bytes).await.map_err(io_err)?;
        f.sync_all().await.map_err(io_err)?;
        fs::rename(&tmp, path).await.map_err(io_err)
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&tmp).await;
    }
    result
}

pub(crate) fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(40);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

// ── helpers ────────────────────────────────────────────────────────────────

impl Storage {
    // -- Job snapshots ------------------------------------------------------

    /// Persist a job's snapshot to `jobs/<id>.json` (atomic). Job ids are
    /// self-generated (hex millis + counter), but validate anyway so a
    /// hand-crafted id can never traverse.
    pub async fn save_job_snapshot(&self, snap: &crate::jobs::JobSnapshot) -> Result<(), ApiError> {
        if !is_safe_job_id(&snap.job_id) {
            return Err(ApiError::BadRequest("invalid job id".into()));
        }
        let dir = self.root.join("jobs");
        fs::create_dir_all(&dir).await.map_err(io_err)?;
        let bytes = serde_json::to_vec_pretty(snap)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("job snapshot encode: {e}")))?;
        atomic_write(&dir.join(format!("{}.json", snap.job_id)), &bytes).await
    }

    pub async fn load_job_snapshot(
        &self,
        job_id: &str,
    ) -> Result<Option<crate::jobs::JobSnapshot>, ApiError> {
        if !is_safe_job_id(job_id) {
            return Err(ApiError::BadRequest("invalid job id".into()));
        }
        let path = self.root.join("jobs").join(format!("{job_id}.json"));
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Startup sweep: a snapshot still `running` belonged to a process that no
    /// longer exists -- mark it failed with an explicit line, so a client
    /// polling the id learns the truth instead of waiting forever. Then prune
    /// to the newest `keep` snapshots (ids are zero-padded millis + counter,
    /// so lexical order is chronological).
    pub async fn sweep_job_snapshots(&self, keep: usize) -> Result<usize, ApiError> {
        let dir = self.root.join("jobs");
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => return Ok(0), // no jobs dir yet
        };
        let mut ids: Vec<String> = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
            if let Some(name) = entry.file_name().to_str()
                && let Some(stem) = name.strip_suffix(".json")
            {
                ids.push(stem.to_string());
            }
        }
        let mut interrupted = 0usize;
        for id in &ids {
            let Some(mut snap) = self.load_job_snapshot(id).await? else {
                continue;
            };
            if snap.status == crate::jobs::Status::Running {
                snap.status = crate::jobs::Status::Failed;
                snap.log.push("interrupted by service restart".to_string());
                self.save_job_snapshot(&snap).await?;
                interrupted += 1;
            }
        }
        ids.sort();
        if ids.len() > keep {
            for id in &ids[..ids.len() - keep] {
                let _ = fs::remove_file(dir.join(format!("{id}.json"))).await;
            }
        }
        Ok(interrupted)
    }
}

/// Job ids as `JobRegistry::create` mints them: lowercase hex + `-`.
fn is_safe_job_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase() || c == '-')
}

/// The slice of a manifest the version listing needs: identity, timestamp,
/// fingerprint, and the array LENGTHS. `IgnoredAny` elements make serde count
/// `mods`/`assets` without materializing entries, so listing a pack's history
/// stays cheap no matter how large its manifests grow.
#[derive(serde::Deserialize)]
struct ManifestHead {
    pack_version: String,
    #[serde(default)]
    channel: Option<VersionChannel>,
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    changelog_i18n: Option<std::collections::BTreeMap<String, String>>,
    generated_at: String,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    built_from: Option<String>,
    minecraft: MinecraftSpec,
    loader: LoaderSpec,
    #[serde(default)]
    mods: Vec<SizedEntry>,
    #[serde(default)]
    assets: Vec<SizedEntry>,
}

/// A manifest entry read for its weight alone: counting the list and adding up
/// what it downloads are the same pass, so the listing takes both from it.
#[derive(serde::Deserialize)]
struct SizedEntry {
    #[serde(default)]
    size_bytes: u64,
}

/// The stored channel wins; a manifest from before the field falls back to
/// the legacy string rule.
fn build_info_from_head(head: ManifestHead) -> ManifestBuildInfo {
    let weigh = |entries: &[SizedEntry]| entries.iter().map(|e| e.size_bytes).sum::<u64>();
    ManifestBuildInfo {
        version_type: head
            .channel
            .unwrap_or_else(|| legacy_version_channel(&head.pack_version)),
        version_number: head.pack_version,
        date_published: head.generated_at,
        fingerprint: head.fingerprint,
        built_from: head.built_from,
        changelog: head.changelog,
        changelog_i18n: head.changelog_i18n,
        mods_count: head.mods.len() as u64,
        assets_count: head.assets.len() as u64,
        minecraft_version: head.minecraft.version,
        loader: head.loader,
        size_bytes: weigh(&head.mods) + weigh(&head.assets),
    }
}

fn io_err(e: std::io::Error) -> ApiError {
    ApiError::Internal(anyhow::Error::from(e))
}

fn json_err(e: serde_json::Error) -> ApiError {
    ApiError::Internal(anyhow::anyhow!("JSON parse error: {e}"))
}

/// Read `summary.json` from each immediate child directory of `dir`, pushing the
/// parsed summaries. Missing dir -> no-op; an unreadable / invalid summary is
/// skipped with a warning. Shared by the official and community listing passes.
async fn read_summaries_in(dir: &Path, out: &mut Vec<PackSummary>) -> Result<(), ApiError> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
        if !entry.file_type().await.map_err(io_err)?.is_dir() {
            continue;
        }
        let summary_path = entry.path().join("summary.json");
        if let Ok(bytes) = fs::read(&summary_path).await {
            match serde_json::from_slice::<PackSummary>(&bytes) {
                Ok(s) => out.push(s),
                Err(e) => {
                    tracing::warn!(path = %summary_path.display(), error = %e, "skipping invalid summary");
                }
            }
        }
    }
    Ok(())
}

/// Push the id of each immediate child directory of `dir` that carries an
/// `authoring/config.json`, prefixed -- so community ids come back as the full
/// `u/<uid>/<pack>` key, official ids bare.
async fn read_authoring_ids_in(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), ApiError> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    while let Some(entry) = entries.next_entry().await.map_err(io_err)? {
        if !entry.file_type().await.map_err(io_err)?.is_dir() {
            continue;
        }
        let cfg = entry.path().join("authoring").join("config.json");
        if fs::metadata(&cfg).await.is_ok() {
            out.push(format!("{prefix}{}", entry.file_name().to_string_lossy()));
        }
    }
    Ok(())
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The cache shard for a content hash: the first two hex chars of the sha1.
/// Both the on-disk layout and the public cache URL bucket by this, so the
/// sharding width has one definition. Precondition: a validated (>= 2 char) sha1.
pub(crate) fn sha1_shard(sha1: &str) -> &str {
    sha1.get(..2).unwrap_or(sha1)
}

/// The one definition of the content-addressed cache layout: a jar with the
/// given sha1 lives at `<root>/cache/<sha1[..2]>/<sha1>.jar`. Both the HTTP and
/// authoring layers build their cache paths on this, so the sharding scheme has
/// a single source of truth. `None` for a non-40-hex sha1.
/// The image kinds `jar_icon` can hand back, as `(extension, content type)`.
/// One home for the mapping both directions.
/// SVG is deliberately absent: an svg served from our own origin is a document
/// that can carry script, and these bytes come from somewhere else. A project
/// whose icon is an svg falls back to the jar's icon or a letter, which is what
/// a missing icon has always done.
const ICON_KINDS: [(&str, &str); 4] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];
/// Marker extension for "this jar was opened and carries no icon".
const NO_ICON: &str = "none";
/// How long a proxied GitHub avatar is served before it is fetched again --
/// the same day the response's own `Cache-Control` gives a browser.
const AVATAR_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(86_400);
/// How long a project icon copied from upstream is served before it is fetched
/// again. A jar's icon cannot change -- the jar is its hash -- but a project's
/// can, so this one has a shelf life.
const PROJECT_ICON_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

/// What the icon cache knows about a jar.
pub enum IconRead {
    Image {
        bytes: Vec<u8>,
        content_type: &'static str,
    },
    /// Opened before, and there is no icon in it.
    Absent,
    /// Never opened.
    Unknown,
}

pub(crate) fn cache_jar_path_in(root: &Path, sha1: &str) -> Option<PathBuf> {
    if sha1.len() != 40 || !is_hex(sha1) {
        return None;
    }
    Some(
        root.join("cache")
            .join(sha1_shard(sha1))
            .join(format!("{sha1}.jar")),
    )
}

// Pack ID / server ID safety: no path traversal, no leading dots, basic alnum +
// dashes + dots only. Stops `..`, absolute paths, and silly characters before
// they reach the filesystem layer.
/// A pack id is either a flat official id (`<seg>`) or a namespaced community id
/// (`u/<uid>/<pack>`, uid all-digits). Both leaf segments are conservative and
/// cannot traverse (no leading dot, no slash); the literal `u` is reserved as the
/// community namespace so an official pack can never collide with `packs/u/`.
pub(crate) fn is_safe_id(s: &str) -> bool {
    match s.strip_prefix("u/") {
        Some(rest) => match rest.split_once('/') {
            Some((uid, pack)) => {
                !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()) && is_flat_id(pack)
            }
            None => false,
        },
        None => s != "u" && is_flat_id(s),
    }
}

/// One conservative path segment: non-empty, <=64, no leading dot (so no `..`),
/// alphanumeric plus `-_.`.
fn is_flat_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn is_safe_version(s: &str) -> bool {
    is_safe_id(s)
}

/// A commit id is exactly what `make_commit` produces: 40 hex characters. Held
/// to that shape rather than to `is_safe_id` because it reaches the filesystem
/// as a name from a URL, and a content address that is not one is not a commit
/// this mirror wrote.
fn is_commit_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reject path traversal and other surprises in user-supplied relative paths.
/// Allows nested directories so a pack's own files (`_pack/banner.png`, and
/// `_nexira/banner.png` for a pack that predates the rename) work,
/// but every segment must be a plain `is_safe_id`-style token and there must
/// be no `..`, `.`, leading slashes, or empty segments.
fn validate_rel_path(rel: &str) -> Result<&str, ApiError> {
    if is_safe_rel_path(rel) {
        Ok(rel)
    } else {
        Err(ApiError::BadRequest("invalid rel_path".into()))
    }
}

/// Boolean core of [validate_rel_path], shared with the authoring layer so its
/// curator- and archive-driven writes reject the same traversal the HTTP layer
/// does. Allows nested dirs and real-world resourcepack/shaderpack filenames
/// (spaces, parens, plus, comma, square brackets) but forbids `..`, leading dots per segment,
/// absolute paths, and backslashes. ASCII-only -- non-ASCII filenames break
/// some Forge launchers on Windows.
pub(crate) fn is_safe_rel_path(rel: &str) -> bool {
    if rel.is_empty() || rel.len() > 512 {
        return false;
    }
    if rel.starts_with('/') || rel.contains('\\') {
        return false;
    }
    rel.split('/').all(|segment| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && segment.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')' | '+' | ',' | '[' | ']')
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LoaderSpec, PackConfig};

    #[test]
    fn safe_id_rejects_traversal() {
        // the guard added to the public pack-read paths leans on this
        assert!(!is_safe_id("../x"));
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("../../etc/passwd"));
        assert!(!is_safe_id("a/b"));
        assert!(is_safe_id("Industrial"));
        assert!(is_safe_id("pack_1.0"));
    }

    #[test]
    fn rel_path_rejects_traversal_accepts_real_filenames() {
        assert!(is_safe_rel_path("_nexira/icon.png"));
        assert!(is_safe_rel_path("resourcepacks/Chocapic13 V7.1 High.zip"));
        assert!(is_safe_rel_path("config/foamfix.cfg"));
        // real resourcepack names carry square-bracketed version ranges; the URL
        // layer percent-encodes them, so the dest validator must allow them too
        assert!(is_safe_rel_path(
            "resourcepacks/NewDefault+v1.82[MC1.9-1.12.2].zip"
        ));
        assert!(!is_safe_rel_path("../etc/passwd"));
        assert!(!is_safe_rel_path("a/../../b"));
        assert!(!is_safe_rel_path("/abs/path"));
        assert!(!is_safe_rel_path("a\\b"));
        assert!(!is_safe_rel_path(".hidden/x"));
        assert!(!is_safe_rel_path("ok/.././bad"));
        assert!(!is_safe_rel_path(""));
    }

    fn sample_config() -> PackConfig {
        PackConfig {
            pack_id: "Industrial".into(),
            display_name: "Industrial".into(),
            tagline: String::new(),
            minecraft_version: "1.12.2".into(),
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2922".into(),
            },
            java_major: 8,
            version: None,
            auth: None,
            tags: vec![],
            featured: false,
            mods: vec![],
            assets: vec![],
            pack_meta: Default::default(),
            owner: 211033194,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    // #146: auto-numbering never collides, so this only fires when a version is
    // named by hand -- and then it is worth stopping, because a rewritten
    // version changes what anyone already holding it downloaded under that name.
    #[tokio::test]
    async fn republishing_a_version_is_refused_unless_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let first = manifest("0.1.0", "2026-05-22T00:00:00Z", 1);
        s.save_manifest("Industrial", &first, false).await.unwrap();

        let second = manifest("0.1.0", "2026-05-23T00:00:00Z", 2);
        let err = s
            .save_manifest("Industrial", &second, false)
            .await
            .expect_err("the version is taken");
        assert!(
            matches!(err, ApiError::Conflict(ref m) if m.contains("0.1.0")),
            "names the version: {err}"
        );
        // and what was published is untouched by the refusal
        assert_eq!(
            s.load_manifest_version("Industrial", "0.1.0")
                .await
                .unwrap()
                .generated_at,
            "2026-05-22T00:00:00Z"
        );

        // asked for explicitly, it goes through
        s.save_manifest("Industrial", &second, true).await.unwrap();
        assert_eq!(
            s.load_manifest_version("Industrial", "0.1.0")
                .await
                .unwrap()
                .generated_at,
            "2026-05-23T00:00:00Z"
        );
    }

    #[tokio::test]
    async fn pack_config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.save_pack_config("Industrial", &sample_config())
            .await
            .unwrap();
        let loaded = s.load_pack_config("Industrial").await.unwrap();
        assert_eq!(loaded.pack_id, "Industrial");
        assert_eq!(loaded.minecraft_version, "1.12.2");
    }

    /// Commit `cfg` onto `parent` and hand back the stored commit.
    async fn commit(
        s: &Storage,
        cfg: &PackConfig,
        parent: Option<String>,
        message: &str,
    ) -> crate::authoring::Commit {
        let (c, snap) = crate::authoring::make_commit(
            cfg,
            parent,
            "operator",
            message,
            vec!["operator".into()],
            format!("2026-07-29T00:00:0{}Z", message.len() % 10),
        )
        .unwrap();
        s.save_commit("Industrial", &c, &snap).await.unwrap();
        c
    }

    #[tokio::test]
    async fn a_commit_round_trips_and_moves_head() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        assert!(s.commit_head("Industrial").await.is_none());

        let c = commit(&s, &sample_config(), None, "first").await;
        assert_eq!(s.commit_head("Industrial").await.as_deref(), Some(&*c.id));
        assert_eq!(s.load_commit("Industrial", &c.id).await.unwrap(), c);
        assert_eq!(
            s.load_commit_config("Industrial", &c.id)
                .await
                .unwrap()
                .minecraft_version,
            "1.12.2"
        );
    }

    #[tokio::test]
    async fn the_log_walks_parents_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let mut cfg = sample_config();
        let first = commit(&s, &cfg, None, "first").await;
        cfg.tagline = "second".into();
        let second = commit(&s, &cfg, Some(first.id.clone()), "second state").await;

        let log = s.commit_log("Industrial", None, 100).await.unwrap();
        assert_eq!(
            log.iter().map(|c| &*c.id).collect::<Vec<_>>(),
            vec![&*second.id, &*first.id]
        );
        // an older state stays readable; that is what makes a restore possible
        assert_eq!(
            s.load_commit_config("Industrial", &first.id)
                .await
                .unwrap()
                .tagline,
            ""
        );
    }

    #[tokio::test]
    async fn publishing_a_build_moves_what_the_history_says_shipped() {
        // the index is derived from the manifests, so writing one must drop it
        // -- a checkpoint that just shipped reading as "never built" is worse
        // than the read it saves
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let c = commit(&s, &sample_config(), None, "first").await;
        assert!(s.builds_by_commit("Industrial").await.is_empty());

        let mut built_manifest = manifest("0.1.0", "2026-08-11T00:00:00Z", 1);
        built_manifest.built_from = Some(c.id.clone());
        s.save_manifest("Industrial", &built_manifest, false)
            .await
            .unwrap();

        let built = s.builds_by_commit("Industrial").await;
        assert_eq!(
            built.get(&c.id).map(Vec::as_slice),
            Some(&["0.1.0".to_string()][..])
        );
    }

    #[tokio::test]
    async fn a_page_continues_where_the_last_one_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let mut cfg = sample_config();
        let mut ids = Vec::new();
        let mut parent = None;
        for n in 0..5 {
            cfg.tagline = format!("take {n}");
            let c = commit(&s, &cfg, parent.clone(), &format!("take {n}")).await;
            parent = Some(c.id.clone());
            ids.push(c.id);
        }
        ids.reverse(); // the log answers newest first

        let first = s.commit_log("Industrial", None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|c| &*c.id).collect::<Vec<_>>(),
            vec![&*ids[0], &*ids[1]]
        );
        let second = s
            .commit_log("Industrial", Some(&first[1].id), 2)
            .await
            .unwrap();
        assert_eq!(
            second.iter().map(|c| &*c.id).collect::<Vec<_>>(),
            vec![&*ids[2], &*ids[3]]
        );
        // and the walk ends where the history does
        let last = s
            .commit_log("Industrial", Some(&second[1].id), 2)
            .await
            .unwrap();
        assert_eq!(
            last.iter().map(|c| &*c.id).collect::<Vec<_>>(),
            vec![&*ids[4]]
        );
        assert!(
            s.commit_log("Industrial", Some(&ids[4]), 2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_cursor_naming_no_commit_ends_the_walk() {
        // answering from the head again would look like the log looping rather
        // than ending
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        commit(&s, &sample_config(), None, "only").await;
        let rows = s
            .commit_log("Industrial", Some(&"f".repeat(40)), 10)
            .await
            .unwrap();
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[tokio::test]
    async fn the_same_state_committed_twice_is_two_commits() {
        // The id covers the parent, so a snapshot identical to its predecessor
        // is still a distinct checkpoint -- otherwise "I committed and nothing
        // happened" would be indistinguishable from a lost write.
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let cfg = sample_config();
        let first = commit(&s, &cfg, None, "first").await;
        let again = commit(&s, &cfg, Some(first.id.clone()), "first").await;
        assert_ne!(first.id, again.id);
        assert_eq!(
            s.commit_log("Industrial", None, 100).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn committing_clears_the_pending_authors() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.note_pending_author("Industrial", "someone")
            .await
            .unwrap();
        s.note_pending_author("Industrial", "someone")
            .await
            .unwrap();
        s.note_pending_author("Industrial", "another")
            .await
            .unwrap();
        // a set, not a log: forty saves by one person is one contributor
        assert_eq!(
            s.pending_authors("Industrial").await,
            ["someone", "another"]
        );

        commit(&s, &sample_config(), None, "first").await;
        assert!(s.pending_authors("Industrial").await.is_empty());
    }

    #[tokio::test]
    async fn a_commit_id_from_outside_cannot_reach_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        // ids arrive in a URL path segment, so they are held to the shape
        // `make_commit` produces rather than to a general path check
        for bad in ["../../etc/passwd", "HEAD", "", &"f".repeat(41)] {
            assert!(s.load_commit("Industrial", bad).await.is_err());
            assert!(s.load_commit_config("Industrial", bad).await.is_err());
        }
    }

    #[tokio::test]
    async fn list_authoring_packs_finds_configured_pack() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        assert!(s.list_authoring_packs().await.unwrap().is_empty());
        s.save_pack_config("Industrial", &sample_config())
            .await
            .unwrap();
        assert_eq!(
            s.list_authoring_packs().await.unwrap(),
            vec!["Industrial".to_string()]
        );
    }

    fn manifest(version: &str, generated_at: &str, mods: usize) -> PackManifest {
        PackManifest {
            schema_version: 2,
            pack_id: "Industrial".into(),
            pack_version: version.into(),
            channel: None,
            changelog: None,
            changelog_i18n: None,
            generated_at: generated_at.into(),
            fingerprint: Some(format!("fp-{version}")),
            checks: None,
            built_from: None,
            minecraft: MinecraftSpec {
                version: "1.12.2".into(),
            },
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2922".into(),
            },
            java: JavaSpec { major: 8 },
            auth: None,
            mods: (0..mods)
                .map(|i| ModEntry {
                    filename: format!("m{i}.jar"),
                    sha1: format!("sha{i}"),
                    size_bytes: 1,
                    required: true,
                    default_enabled: true,
                    source: Source::SmrtCache { url: "u".into() },
                    display: None,
                    slug: None,
                })
                .collect(),
            assets: vec![],
        }
    }

    #[tokio::test]
    async fn manifest_builds_carry_metadata_newest_first_and_resolve_latest() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        // an operator release, then two panel snapshots on a later day
        s.save_manifest(
            "Industrial",
            &manifest("2026.05.22.2", "2026-05-22T10:00:00Z", 3),
            false,
        )
        .await
        .unwrap();
        s.save_manifest(
            "Industrial",
            &manifest("SNAPSHOT-0.0.0-2026.07.18", "2026-07-18T09:00:00Z", 4),
            false,
        )
        .await
        .unwrap();
        s.save_manifest(
            "Industrial",
            &manifest("SNAPSHOT-0.0.0-2026.07.18.1", "2026-07-18T11:00:00Z", 5),
            false,
        )
        .await
        .unwrap();
        s.set_latest_manifest("Industrial", "SNAPSHOT-0.0.0-2026.07.18.1")
            .await
            .unwrap();

        let builds = s.list_manifest_builds("Industrial").await.unwrap();
        let versions: Vec<&str> = builds.iter().map(|b| b.version_number.as_str()).collect();
        assert_eq!(
            versions,
            vec![
                "SNAPSHOT-0.0.0-2026.07.18.1",
                "SNAPSHOT-0.0.0-2026.07.18",
                "2026.05.22.2"
            ],
            "newest first by built_at"
        );
        // no stored channel on these -> the legacy string rule applies
        assert_eq!(builds[0].version_type, VersionChannel::Beta);
        assert_eq!(builds[2].version_type, VersionChannel::Release);
        assert_eq!(builds[0].mods_count, 5);
        assert_eq!(
            builds[0].fingerprint.as_deref(),
            Some("fp-SNAPSHOT-0.0.0-2026.07.18.1")
        );
        assert_eq!(builds[0].date_published, "2026-07-18T11:00:00Z");

        assert_eq!(
            s.latest_manifest_version("Industrial").await.unwrap(),
            Some("SNAPSHOT-0.0.0-2026.07.18.1".to_string())
        );
        let info = s.latest_build_info("Industrial").await.unwrap().unwrap();
        assert_eq!(info.version_number, "SNAPSHOT-0.0.0-2026.07.18.1");
        assert_eq!(info.version_type, VersionChannel::Beta);

        // a pack with no builds yields no latest, not an error
        assert_eq!(s.latest_manifest_version("Ghost").await.unwrap(), None);
        assert!(s.latest_build_info("Ghost").await.unwrap().is_none());
    }

    // Extracting an icon means reading a whole jar and walking a zip for a few
    // kilobytes, and the answer cannot change: the jar is named by its own hash.
    #[tokio::test]
    async fn an_icon_is_extracted_once_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let sha1 = "a".repeat(40);
        assert!(matches!(s.read_cached_icon(&sha1).await, IconRead::Unknown));

        s.store_icon(&sha1, Some((b"\x89PNG-ish", "image/png")))
            .await;
        match s.read_cached_icon(&sha1).await {
            IconRead::Image {
                bytes,
                content_type,
            } => {
                assert_eq!(bytes, b"\x89PNG-ish");
                assert_eq!(content_type, "image/png");
            }
            _ => panic!("the icon was stored, so it should read back"),
        }

        // and the type it was stored under survives the round trip
        let jpg = "b".repeat(40);
        s.store_icon(&jpg, Some((b"jpeg-ish", "image/jpeg"))).await;
        assert!(matches!(
            s.read_cached_icon(&jpg).await,
            IconRead::Image {
                content_type: "image/jpeg",
                ..
            }
        ));
    }

    // Most jars carry no icon at all. Not remembering that is the expensive
    // case, not the cheap one: every request would re-read the jar to find the
    // same nothing.
    #[tokio::test]
    async fn a_jar_with_no_icon_is_remembered_as_having_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let sha1 = "c".repeat(40);
        s.store_icon(&sha1, None).await;
        assert!(matches!(s.read_cached_icon(&sha1).await, IconRead::Absent));
    }

    // A takedown removes the jar; the picture that came out of it must not
    // outlive it.
    #[tokio::test]
    async fn a_takedown_takes_the_icon_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let sha1 = sha1_hex(b"jar");
        s.save_cache_jar(&sha1, b"jar").await.unwrap();
        s.store_icon(&sha1, Some((b"png-ish", "image/png"))).await;

        s.takedown(&sha1).await.unwrap();
        assert!(!s.has_cache_jar(&sha1).await);
        assert!(
            matches!(s.read_cached_icon(&sha1).await, IconRead::Unknown),
            "a taken-down jar keeps serving its icon"
        );
    }

    // A project icon is a few kilobytes of picture, not the mod: holding it is
    // what keeps a viewer's browser from announcing itself to somebody else's
    // CDN once per icon on the page.
    #[tokio::test]
    async fn a_project_icon_is_held_and_ages_out() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        assert!(s.read_project_icon("AANobbMI").await.is_none());

        s.store_project_icon("AANobbMI", b"webp-ish", "image/webp")
            .await;
        let (bytes, content_type) = s.read_project_icon("AANobbMI").await.unwrap();
        assert_eq!(bytes, b"webp-ish");
        assert_eq!(content_type, "image/webp");

        // a copy older than the shelf life reads as absent, so it is fetched
        // again rather than served forever from whenever it was first taken
        let held = dir.path().join("icons/modrinth/AANobbMI.webp");
        let long_ago = std::time::SystemTime::now()
            - PROJECT_ICON_MAX_AGE
            - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&held)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert!(
            s.read_project_icon("AANobbMI").await.is_none(),
            "a copy past its shelf life must not be served"
        );
    }

    // An svg from somewhere else, served from our own origin, is a document that
    // can carry script. It is not stored, so it is never served back.
    #[tokio::test]
    async fn an_icon_kind_the_mirror_will_not_serve_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.store_project_icon("AANobbMI", b"<svg onload=alert(1)>", "image/svg+xml")
            .await;
        assert!(s.read_project_icon("AANobbMI").await.is_none());
    }

    // The id becomes a filename, so anything that could leave the directory --
    // or is not a plausible handle at all -- gets no path.
    #[tokio::test]
    async fn a_project_id_that_is_not_a_handle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        for bad in ["../../etc/passwd", "a/b", "", "with space", "dot.dot"] {
            s.store_project_icon(bad, b"png-ish", "image/png").await;
            assert!(
                s.read_project_icon(bad).await.is_none(),
                "{bad} must not become a path"
            );
        }
    }

    // Answering "do we hold this jar" by listing the cache made one boolean cost
    // a walk of every directory in it. The path is the hash, so the question has
    // an answer of its own size.
    #[tokio::test]
    async fn a_jar_is_looked_up_by_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let held = sha1_hex(b"jar");
        let absent = "b".repeat(40);
        s.save_cache_jar(&held, b"jar").await.unwrap();

        assert!(s.has_cache_jar(&held).await);
        assert_eq!(s.cache_jar_size(&held).await, Some(3));
        assert_eq!(s.cache_jar_size(&absent).await, None);
        assert!(!s.has_cache_jar(&absent).await);
        // not a hash at all: an answer, not a panic
        assert!(!s.has_cache_jar("../../etc/passwd").await);
        assert!(!s.has_cache_jar("").await);

        // and the same question asked about a known set of artifacts
        let among = s
            .cached_among(vec![held.clone(), absent.clone(), held.clone()])
            .await;
        assert_eq!(among, [held].into_iter().collect::<HashSet<_>>());
        assert!(s.cached_among(Vec::<String>::new()).await.is_empty());
    }

    // A jar sits under the first two characters of its own hash, so a page of
    // the inventory opens the directories from the cursor on and stops when it
    // is full, rather than walking the whole cache to throw most of it away.
    #[tokio::test]
    async fn the_cache_inventory_reads_a_page_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let shas: Vec<String> = ["00", "01", "7f", "aa", "ff"]
            .iter()
            .map(|prefix| format!("{prefix}{}", "0".repeat(38)))
            .collect();
        for sha1 in &shas {
            let prefix = dir.path().join("cache").join(&sha1[..2]);
            std::fs::create_dir_all(&prefix).unwrap();
            std::fs::write(prefix.join(format!("{sha1}.jar")), b"jar").unwrap();
        }

        let whole = s.list_cache_inventory().await.unwrap();
        assert_eq!(
            whole.iter().map(|e| e.sha1.clone()).collect::<Vec<_>>(),
            shas,
            "unpaged, the inventory is the whole cache in hash order"
        );

        let mut walked = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = s
                .cache_inventory_after(after.as_deref(), Some(2))
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 2);
            after = page.last().map(|e| e.sha1.clone());
            walked.extend(page.into_iter().map(|e| e.sha1));
        }
        assert_eq!(
            walked, shas,
            "paged, it is the same cache in the same order"
        );
    }

    // What a build targets, and what installing it costs, are read off the
    // listing -- otherwise showing "this update moves you to 1.20.1" means
    // fetching every manifest in the list to find out.
    #[tokio::test]
    async fn a_build_says_what_it_targets_and_what_it_weighs() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let mut m = manifest("1.0.0", "2026-07-30T10:00:00Z", 3);
        for (i, entry) in m.mods.iter_mut().enumerate() {
            entry.size_bytes = 1_000 * (i as u64 + 1);
        }
        m.assets.push(AssetEntry {
            dest: "config/foo.cfg".into(),
            sha1: "sha-asset".into(),
            size_bytes: 500,
            required: true,
            source: Source::SmrtCache { url: "u".into() },
            display: None,
        });
        s.save_manifest("Industrial", &m, false).await.unwrap();

        let builds = s.list_manifest_builds("Industrial").await.unwrap();
        let build = &builds[0];
        assert_eq!(build.minecraft_version, "1.12.2");
        assert_eq!(build.loader.name, "forge");
        assert_eq!(build.loader.version, "14.23.5.2922");
        // every file the build lists, mods and assets alike
        assert_eq!(build.size_bytes, 1_000 + 2_000 + 3_000 + 500);
        assert_eq!(build.assets_count, 1);
    }

    #[tokio::test]
    async fn stored_channel_wins_over_the_legacy_string_rule() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        // a modern build: semver label, channel stored on the manifest
        let mut m = manifest("0.0.3", "2026-07-19T10:00:00Z", 2);
        m.channel = Some(VersionChannel::Alpha);
        s.save_manifest("Industrial", &m, false).await.unwrap();
        s.set_latest_manifest("Industrial", "0.0.3").await.unwrap();

        let info = s.latest_build_info("Industrial").await.unwrap().unwrap();
        assert_eq!(info.version_number, "0.0.3");
        assert_eq!(
            info.version_type,
            VersionChannel::Alpha,
            "the stored channel wins; the string rule would have said release"
        );
    }

    #[tokio::test]
    async fn job_snapshot_sweep_marks_orphans_and_prunes() {
        use crate::jobs::{JobSnapshot, Status};
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        let snap = |id: &str, status: Status| JobSnapshot {
            job_id: id.into(),
            kind: "build".into(),
            pack_id: "Industrial".into(),
            status,
            log: vec!["building".into()],
        };
        // oldest done, middle running (orphan), newest done
        s.save_job_snapshot(&snap("0000000000001-00000000", Status::Done))
            .await
            .unwrap();
        s.save_job_snapshot(&snap("0000000000002-00000000", Status::Running))
            .await
            .unwrap();
        s.save_job_snapshot(&snap("0000000000003-00000000", Status::Done))
            .await
            .unwrap();

        let interrupted = s.sweep_job_snapshots(2).await.unwrap();
        assert_eq!(interrupted, 1, "one orphaned running job");

        // the orphan is now failed, with the reason on its log
        let orphan = s
            .load_job_snapshot("0000000000002-00000000")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(orphan.status, Status::Failed);
        assert_eq!(
            orphan.log.last().map(String::as_str),
            Some("interrupted by service restart")
        );
        // pruned to the newest two: the oldest snapshot is gone
        assert!(
            s.load_job_snapshot("0000000000001-00000000")
                .await
                .unwrap()
                .is_none(),
            "oldest pruned past the keep bound"
        );
        assert!(
            s.load_job_snapshot("0000000000003-00000000")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn manifest_version_strings_sort_by_tuple_not_string() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        for v in ["2026.05.22.10", "2026.05.22.2"] {
            s.save_manifest("Industrial", &manifest(v, "2026-05-22T00:00:00Z", 1), false)
                .await
                .unwrap();
        }
        assert_eq!(
            s.list_manifest_versions("Industrial").await.unwrap(),
            vec!["2026.05.22.2".to_string(), "2026.05.22.10".to_string()],
            ".10 lands after .2 under tuple comparison"
        );
    }

    #[tokio::test]
    async fn duplicate_pack_clones_config_and_static_with_loader_override() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.save_pack_config("Industrial", &sample_config())
            .await
            .unwrap();
        s.save_pack_static("Industrial", "config/foamfix.cfg", b"a=1")
            .await
            .unwrap();

        let cleanroom = LoaderSpec {
            name: "cleanroom".into(),
            version: "0.2.3".into(),
        };
        let new_cfg = s
            .duplicate_pack(
                "Industrial",
                "Industrial-cleanroom",
                Some(cleanroom),
                7,
                None,
            )
            .await
            .unwrap();

        // returned + persisted config carries the new id and the overridden loader
        assert_eq!(new_cfg.pack_id, "Industrial-cleanroom");
        assert_eq!(new_cfg.loader.name, "cleanroom");
        // a duplicate is a fresh draft owned by the cloner, not the source
        assert_eq!(new_cfg.owner, 7);
        assert_eq!(new_cfg.visibility, Visibility::Draft);
        assert!(new_cfg.fork_of.is_none());
        let loaded = s.load_pack_config("Industrial-cleanroom").await.unwrap();
        assert_eq!(loaded.loader.version, "0.2.3");

        // static tree copied verbatim
        assert_eq!(
            s.list_pack_static("Industrial-cleanroom").await.unwrap(),
            vec!["config/foamfix.cfg".to_string()]
        );

        // source is untouched -- still Forge
        let src = s.load_pack_config("Industrial").await.unwrap();
        assert_eq!(src.loader.name, "forge");
    }

    #[tokio::test]
    async fn duplicate_pack_refuses_existing_target_and_self() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.save_pack_config("Industrial", &sample_config())
            .await
            .unwrap();
        s.save_pack_config("Taken", &sample_config()).await.unwrap();

        assert!(matches!(
            s.duplicate_pack("Industrial", "Taken", None, 1, None).await,
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            s.duplicate_pack("Industrial", "Industrial", None, 1, None)
                .await,
            Err(ApiError::BadRequest(_))
        ));
        // unknown source -> NotFound (from the underlying config load)
        assert!(matches!(
            s.duplicate_pack("Nope", "Fresh", None, 1, None).await,
            Err(ApiError::NotFound)
        ));
    }

    #[test]
    fn is_safe_id_accepts_official_and_community_and_blocks_traversal() {
        assert!(is_safe_id("Industrial"));
        assert!(is_safe_id("pack_1-2.3"));
        assert!(is_safe_id("u/211033194/CoolPack"));
        // reserved namespace + malformed community keys
        assert!(!is_safe_id("u"));
        assert!(!is_safe_id("u/211033194")); // no pack segment
        assert!(!is_safe_id("u/abc/CoolPack")); // non-digit uid
        assert!(!is_safe_id("u//CoolPack")); // empty uid
        // no traversal in either form
        assert!(!is_safe_id(".."));
        assert!(!is_safe_id("u/1/.."));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(""));
    }

    fn sample_summary(pack_id: &str) -> PackSummary {
        let json = format!(
            r#"{{"pack_id":"{pack_id}","display_name":"X","tagline":"",
                 "minecraft_version":"1.12.2","latest_pack_version":"1","tags":[]}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[tokio::test]
    async fn list_pack_summaries_includes_community_packs() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(dir.path().to_path_buf());
        s.save_pack_summary(&sample_summary("Industrial"))
            .await
            .unwrap();
        s.save_pack_summary(&sample_summary("u/42/CoolPack"))
            .await
            .unwrap();

        let ids: Vec<String> = s
            .list_pack_summaries()
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.pack_id)
            .collect();
        assert!(ids.contains(&"Industrial".to_string()), "official listed");
        assert!(
            ids.contains(&"u/42/CoolPack".to_string()),
            "community listed"
        );
    }
}
