use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use smrt::authoring::{
    self, BootstrapArgs, Modrinth, enrich_from_mcmod_info, gate, infer_requires_from_mcmod_info,
};
use smrt::domain::{LoaderSpec, PackConfig, PackManifest, PackSummary, VersionChannel};
use smrt::registry::Registry;
use smrt::storage::Storage;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

const DEFAULT_MIRROR_BASE: &str = "https://smrt.hivens.dev";

#[derive(Parser, Debug)]
#[command(
    name = "smrt-pack",
    version = env!("SMRT_BUILD_VERSION"),
    about = "Authoring CLI for smrt packs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Read an instance archive (any zip with a top-level `mods/`) and emit a
    /// starter PackConfig JSON.
    Bootstrap {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        pack_id: String,
        #[arg(long)]
        display_name: String,
        #[arg(long, default_value = "")]
        tagline: String,
        #[arg(long)]
        minecraft_version: String,
        #[arg(long, default_value = "forge")]
        loader_name: String,
        #[arg(long)]
        loader_version: String,
        #[arg(long, default_value_t = 8)]
        java_major: u32,
        /// Storage root: extracted mod jars land in {storage}/cache/, extras
        /// files land in {storage}/packs/{pack_id}/static/.
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },

    /// Cross-reference a PackConfig against an instance archive by filename.
    Validate {
        #[arg(long)]
        config: PathBuf,
        #[arg(long = "against-archive")]
        archive: PathBuf,
    },

    /// Resolve every source in a PackConfig and write the wire manifest.
    Build {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
        /// Defaults to the next auto-numbered `<base>.<counter>`.
        #[arg(long)]
        pack_version: Option<String>,
        /// Channel stored on the manifest: release | beta | alpha.
        /// Publishing a release is an explicit act, so the default is beta.
        #[arg(long, default_value = "beta")]
        channel: String,
        /// Release notes stored on the manifest (CommonMark).
        #[arg(long)]
        changelog: Option<String>,
        #[arg(long, default_value = DEFAULT_MIRROR_BASE)]
        mirror_base: String,
        /// Publish even though the pre-publish check found something that means
        /// the pack cannot start. Recorded on the manifest.
        #[arg(long)]
        force: bool,
        /// Write over a version that is already published, changing what anyone
        /// who has it already downloaded under that name.
        #[arg(long)]
        overwrite_version: bool,
    },

    /// Pull each declared mod's missing hard dependencies in (Modrinth first,
    /// the mirror's cache second) and record the resolved requires graph in
    /// display.requires -- the same pass the panel runs on config save.
    Depfill {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },

    /// Fill display.name / description / url from each smrt_cache mod's
    /// `mcmod.info`. Existing authored values win. Idempotent.
    EnrichMcmod {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },

    /// Walk each mod's `mcmod.info.dependencies`, resolve modids
    /// against sibling mods in the pack, emit `display.requires`
    /// entries. Existing `requires` lists are preserved.
    InferRequires {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },

    /// PUT a local directory tree into the mirror's pack static area
    /// via the admin API. Walks every regular file under [dir] and
    /// publishes each at `/v1/authoring/packs/<pack_id>/static/<rel_path>`
    /// (relative to dir). Reads the admin token from [token_file]
    /// (default `/tmp/smrt-token`).
    UploadStatic {
        #[arg(long)]
        pack_id: String,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, default_value = DEFAULT_MIRROR_BASE)]
        mirror_base: String,
        #[arg(long, default_value = "/tmp/smrt-token")]
        token_file: PathBuf,
        /// Skip files matching any of these path prefixes (relative
        /// to [dir]). Repeatable. Default skips obvious junk
        /// (`.DS_Store`, `Thumbs.db`).
        #[arg(long = "skip", default_values = [".DS_Store", "Thumbs.db"])]
        skip: Vec<String>,
    },

    /// Reconstruct an editable authoring config from a published manifest +
    /// summary, to migrate a CLI-era pack (no `authoring/` inputs) into the
    /// panel's editable format. Recovers pack-card metadata from the summary.
    ReconstructConfig {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        summary: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },

    /// Mod-identity registry: harvest the cache + manifests into SQLite, or
    /// inspect it.
    Registry {
        #[command(subcommand)]
        sub: RegistryCmd,
    },
}

#[derive(Subcommand, Debug)]
enum RegistryCmd {
    /// Scan the cache + published manifests, read mcmod.info, resolve Modrinth
    /// identity, and reconcile into the registry DB. Idempotent; never
    /// clobbers authored rows.
    Harvest {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },
    /// Print registry counts (mods / versions / relations / packs / builds / orphans).
    Stats {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },
    /// List cached artifacts no build references.
    Orphans {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
    },
    /// Add (or --remove) a mutual authored conflict between two mods, by modid.
    Conflict {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
        #[arg(long)]
        a: String,
        #[arg(long)]
        b: String,
        #[arg(long)]
        remove: bool,
    },
    /// Set (or --remove) an authored jar classification by sha1 -- the
    /// debug escape hatch for a jar the classifier left undecided. Refused
    /// for Modrinth-identified mods (their env flags are authoritative).
    Classify {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
        #[arg(long)]
        sha1: String,
        /// mod | coremod | library
        #[arg(long, default_value = "mod")]
        kind: String,
        /// client | server | both
        #[arg(long)]
        side: Option<String>,
        /// must_match | tolerant
        #[arg(long)]
        policy: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Snapshot the registry DB to a file (VACUUM INTO).
    Backup {
        #[arg(long, default_value = "/var/lib/smrt")]
        storage: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "smrt_pack=info,info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Bootstrap {
            archive: archive_path,
            out,
            pack_id,
            display_name,
            tagline,
            minecraft_version,
            loader_name,
            loader_version,
            java_major,
            storage,
        } => {
            let archive = fs::read(&archive_path)
                .with_context(|| format!("reading {}", archive_path.display()))?;
            let cfg = authoring::bootstrap(
                BootstrapArgs {
                    pack_id,
                    display_name,
                    tagline,
                    minecraft_version,
                    loader: LoaderSpec {
                        name: loader_name,
                        version: loader_version,
                    },
                    java_major,
                    storage,
                },
                archive,
            )
            .await?;
            write_pack_config(&cfg, &out)?;
            info!(
                path = %out.display(),
                mods = cfg.mods.len(),
                assets = cfg.assets.len(),
                "wrote starter config"
            );
            Ok(())
        }
        Cmd::Validate { config, archive } => run_validate(&config, &archive),
        Cmd::Build {
            config,
            storage,
            pack_version,
            channel,
            changelog,
            mirror_base,
            force,
            overwrite_version,
        } => {
            let channel = VersionChannel::parse(&channel)
                .ok_or_else(|| anyhow::anyhow!("channel must be release, beta or alpha"))?;
            run_build(
                &config,
                &storage,
                pack_version.as_deref(),
                channel,
                changelog,
                &mirror_base,
                force,
                overwrite_version,
            )
            .await
        }
        Cmd::Depfill {
            config,
            out,
            storage,
        } => run_depfill(&config, &out, &storage).await,
        Cmd::EnrichMcmod {
            config,
            out,
            storage,
        } => run_enrich_mcmod(&config, &out, &storage),
        Cmd::InferRequires {
            config,
            out,
            storage,
        } => run_infer_requires(&config, &out, &storage),
        Cmd::UploadStatic {
            pack_id,
            dir,
            mirror_base,
            token_file,
            skip,
        } => run_upload_static(&pack_id, &dir, &mirror_base, &token_file, &skip).await,
        Cmd::ReconstructConfig {
            manifest,
            summary,
            out,
        } => run_reconstruct_config(&manifest, &summary, &out),
        Cmd::Registry { sub } => match sub {
            RegistryCmd::Harvest { storage } => run_registry_harvest(&storage).await,
            RegistryCmd::Stats { storage } => run_registry_stats(&storage),
            RegistryCmd::Orphans { storage } => run_registry_orphans(&storage),
            RegistryCmd::Conflict {
                storage,
                a,
                b,
                remove,
            } => run_registry_conflict(&storage, &a, &b, remove),
            RegistryCmd::Classify {
                storage,
                sha1,
                kind,
                side,
                policy,
                remove,
            } => run_registry_classify(
                &storage,
                &sha1,
                &kind,
                side.as_deref(),
                policy.as_deref(),
                remove,
            ),
            RegistryCmd::Backup { storage, out } => run_registry_backup(&storage, &out),
        },
    }
}

// ── registry ────────────────────────────────────────────────────────────────

async fn run_registry_harvest(storage: &Path) -> Result<()> {
    let store = Storage::new(storage.to_path_buf());
    let modrinth = Modrinth::new()?;
    let registry = Arc::new(Registry::open(storage.join("registry.db"))?);
    let report = authoring::harvest::run_harvest(&store, &modrinth, registry).await?;
    info!(
        jars = report.jars_scanned,
        no_identity = report.jars_no_identity,
        mods = report.mods,
        versions = report.mod_versions,
        relations = report.relations,
        builds = report.builds,
        inferred_requires = report.inferred_requires,
        inferred_optional = report.inferred_optional,
        modrinth_deps = report.modrinth_deps,
        declared_deps = report.declared_deps,
        sides = report.sides_derived,
        "harvest complete"
    );
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_registry_stats(storage: &Path) -> Result<()> {
    let registry = Registry::open(storage.join("registry.db"))?;
    let stats = registry.with_conn(smrt::registry::queries::stats)?;
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

fn run_registry_orphans(storage: &Path) -> Result<()> {
    let registry = Registry::open(storage.join("registry.db"))?;
    let orphans = registry.with_conn(smrt::registry::queries::orphan_jars)?;
    for o in &orphans {
        println!(
            "{}  {:>11} B  {}",
            o.sha1,
            o.size_bytes,
            o.filename.as_deref().unwrap_or("(no name)")
        );
    }
    println!("{} orphan(s)", orphans.len());
    Ok(())
}

fn run_registry_conflict(storage: &Path, a: &str, b: &str, remove: bool) -> Result<()> {
    let registry = Registry::open(storage.join("registry.db"))?;
    registry.set_conflict(a, b, remove)?;
    info!(a, b, remove, "set authored conflict");
    Ok(())
}

fn run_registry_classify(
    storage: &Path,
    sha1: &str,
    kind: &str,
    side: Option<&str>,
    policy: Option<&str>,
    remove: bool,
) -> Result<()> {
    let registry = Registry::open(storage.join("registry.db"))?;
    registry.author_jar_class(sha1, kind, side, policy, remove)?;
    info!(
        sha1,
        kind, side, policy, remove, "authored jar classification"
    );
    Ok(())
}

fn run_registry_backup(storage: &Path, out: &Path) -> Result<()> {
    let registry = Registry::open(storage.join("registry.db"))?;
    registry.backup_into(out)?;
    info!(out = %out.display(), "registry backup written");
    Ok(())
}

fn run_reconstruct_config(manifest_path: &Path, summary_path: &Path, out: &Path) -> Result<()> {
    let manifest: PackManifest = read_json(manifest_path)?;
    let summary: PackSummary = read_json(summary_path)?;
    let cfg = authoring::reconstruct_config(&manifest, &summary);
    let json = serde_json::to_vec_pretty(&cfg).context("encoding reconstructed config")?;
    fs::write(out, &json).with_context(|| format!("writing {}", out.display()))?;
    info!(
        out = %out.display(),
        mods = cfg.mods.len(),
        assets = cfg.assets.len(),
        "reconstructed editable config from manifest + summary"
    );
    Ok(())
}

// ── build + validate (thin wrappers over authoring::) ──────────────────────

fn run_validate(config_path: &Path, archive_path: &Path) -> Result<()> {
    let cfg: PackConfig = read_json(config_path)?;
    let archive_bytes =
        fs::read(archive_path).with_context(|| format!("reading {}", archive_path.display()))?;
    let report = authoring::validate(&cfg, &archive_bytes)?;

    println!("instance archive: {} mods", report.archive_mod_count);
    println!(
        "PackConfig: {} mods declared, {} assets declared",
        report.declared_mods, report.declared_assets
    );
    println!("matched by filename: {}", report.matched);

    if !report.missing_in_config.is_empty() {
        println!("\nIn the archive but missing from PackConfig (would break the handshake):");
        for f in &report.missing_in_config {
            println!("  - {}", f);
        }
    }
    if !report.extra_in_config.is_empty() {
        println!(
            "\nIn PackConfig but not in the archive (client additions, expected if intentional):"
        );
        for f in &report.extra_in_config {
            println!("  + {}", f);
        }
    }

    if !report.missing_in_config.is_empty() {
        bail!(
            "{} archive mods missing from PackConfig",
            report.missing_in_config.len()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_build(
    config_path: &Path,
    storage: &Path,
    pack_version: Option<&str>,
    channel: VersionChannel,
    changelog: Option<String>,
    mirror_base: &str,
    force: bool,
    overwrite: bool,
) -> Result<()> {
    let mut cfg: PackConfig = read_json(config_path)?;
    // Build enrichment passes run on a transient copy: fill display metadata
    // from each cache jar's mcmod.info, then infer the requires graph.
    enrich_from_mcmod_info(&mut cfg, storage)?;
    infer_requires_from_mcmod_info(&mut cfg, storage)?;
    // side/policy classification through the registry decision layer
    let registry = Arc::new(Registry::open(storage.join("registry.db"))?);
    let classifications = registry.with_conn(|c| authoring::resolve::classify_pack(c, &cfg))?;

    // The same gate the service applies (#108). This writes into the same
    // storage tree the launcher reads, so it cannot be the way around the check.
    // Unlike the service there is nobody to attribute an override to -- whoever
    // has a shell on the box ran it -- so it is recorded on the manifest and in
    // this process's log, and not in the account audit trail, which is about
    // identities.
    let mut checks =
        registry.with_conn(|c| authoring::resolve_pack(c, &cfg).map(|r| gate::check(&r)))?;
    for line in &checks.blocking {
        warn!("blocking: {line}");
    }
    for line in &checks.advisory {
        info!("noted: {line}");
    }
    if !checks.blocking.is_empty() {
        if !force {
            bail!(
                "{} problem(s) would keep this pack from starting -- fix them, or build again with --force",
                checks.blocking.len()
            );
        }
        checks.overridden = true;
        warn!(
            blocking = checks.blocking.len(),
            "publishing over the pre-publish check, as --force asked"
        );
    }

    let modrinth = Modrinth::new()?;
    let built = authoring::build_manifest(
        &cfg,
        storage,
        pack_version,
        channel,
        changelog,
        // the CLI takes one note; per-language notes are authored where they are
        // written, which is the panel
        None,
        mirror_base,
        &classifications,
        &registry,
        &modrinth,
    )
    .await?;
    let mut manifest = built.manifest;
    manifest.checks = (!checks.is_empty()).then_some(checks);
    let fell_back = built.resolved_from_registry;
    if !fell_back.is_empty() {
        warn!(
            mods = %fell_back.join(", "),
            "Modrinth unreachable; resolved these from the registry"
        );
    }
    let summary = authoring::make_pack_summary(&cfg, &manifest.pack_version);

    let store = Storage::new(storage.to_path_buf());
    store
        .save_manifest(&cfg.pack_id, &manifest, overwrite)
        .await?;
    store
        .set_latest_manifest(&cfg.pack_id, &manifest.pack_version)
        .await?;
    store.save_pack_summary(&summary).await?;
    info!(pack_version = %manifest.pack_version, "build complete");
    Ok(())
}

// ── enrichment subcommands ────────────────────────────────────────────────

async fn run_depfill(config_path: &Path, out_path: &Path, storage: &Path) -> Result<()> {
    let mut cfg: PackConfig = read_json(config_path)?;
    let store = Storage::new(storage.to_path_buf());
    let registry = Arc::new(Registry::open(storage.join("registry.db"))?);
    let modrinth = Modrinth::new()?;
    let added =
        smrt::authoring::depfill::fill_dependencies(&mut cfg, &registry, &modrinth, &store).await?;
    write_pack_config(&cfg, out_path)?;
    info!(added, out = %out_path.display(), "dependency fill complete");
    Ok(())
}

fn run_enrich_mcmod(config_path: &Path, out_path: &Path, storage: &Path) -> Result<()> {
    let mut cfg: PackConfig = read_json(config_path)?;
    enrich_from_mcmod_info(&mut cfg, storage)?;
    write_pack_config(&cfg, out_path)
}

fn run_infer_requires(config_path: &Path, out_path: &Path, storage: &Path) -> Result<()> {
    let mut cfg: PackConfig = read_json(config_path)?;
    let report = infer_requires_from_mcmod_info(&mut cfg, storage)?;
    if !report.edges_skipped_unresolved.is_empty() {
        warn!(
            "{} dependency edge(s) skipped -- modid not found among sibling mods (sample: {:?})",
            report.edges_skipped_unresolved.len(),
            report
                .edges_skipped_unresolved
                .iter()
                .take(5)
                .collect::<Vec<_>>(),
        );
    }
    write_pack_config(&cfg, out_path)
}

fn write_pack_config(cfg: &PackConfig, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let pretty = serde_json::to_string_pretty(cfg)?;
    fs::write(path, pretty).with_context(|| format!("writing {}", path.display()))
}

async fn run_upload_static(
    pack_id: &str,
    dir: &Path,
    mirror_base: &str,
    token_file: &Path,
    skip: &[String],
) -> Result<()> {
    let token = fs::read_to_string(token_file)
        .with_context(|| format!("reading admin token from {}", token_file.display()))?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("admin token file {} is empty", token_file.display());
    }
    if !dir.is_dir() {
        bail!("upload source {} is not a directory", dir.display());
    }
    let client = reqwest::Client::builder()
        .user_agent("Kitty-Hivens/smrt-pack")
        .build()
        .context("building reqwest client")?;

    // The whole tree is walked before anything is sent. The walk is ordinary
    // synchronous directory reading and the uploads are ordinary awaits, so
    // neither has to pretend to be the other -- and the order is stable, which
    // makes the failure report readable against a rerun.
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut skipped = 0usize;
    collect_files_for_upload(dir, dir, skip, &mut files, &mut skipped)?;
    files.sort();

    let mut uploaded = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for (rel_path, abs_path) in files {
        // Path::join with leading separator on Linux silently drops the prefix;
        // explicit format keeps the URL well-formed.
        let url = static_upload_url(mirror_base, pack_id, &rel_path);
        info!(rel = %rel_path, "uploading");
        let body =
            fs::read(&abs_path).with_context(|| format!("reading {}", abs_path.display()))?;
        match client.put(&url).bearer_auth(&token).body(body).send().await {
            Ok(r) if r.status().is_success() => uploaded += 1,
            Ok(r) => failed.push((rel_path, format!("HTTP {}", r.status()))),
            Err(e) => failed.push((rel_path, e.to_string())),
        }
    }

    if !failed.is_empty() {
        warn!(
            "{} upload(s) failed (sample: {:?})",
            failed.len(),
            failed.iter().take(5).collect::<Vec<_>>()
        );
    }
    info!(
        uploaded,
        skipped,
        failed = failed.len(),
        "upload-static complete"
    );
    if !failed.is_empty() {
        bail!(
            "{} of {} uploads failed",
            failed.len(),
            uploaded + failed.len()
        );
    }
    Ok(())
}

/// Every regular file under `here`, as `(path relative to root, absolute
/// path)`, minus whatever `skip` names.
fn collect_files_for_upload(
    root: &Path,
    here: &Path,
    skip: &[String],
    out: &mut Vec<(String, PathBuf)>,
    skipped: &mut usize,
) -> Result<()> {
    let entries = fs::read_dir(here).with_context(|| format!("read_dir {}", here.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_files_for_upload(root, &path, skip, out, skipped)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).with_context(|| {
            format!("relativizing {} against {}", path.display(), root.display())
        })?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if skip
            .iter()
            .any(|s| rel_str.starts_with(s) || rel_str.contains(s))
        {
            *skipped += 1;
            continue;
        }
        out.push((rel_str, path));
    }
    Ok(())
}

fn static_upload_url(base: &str, pack_id: &str, rel_path: &str) -> String {
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
        .add(b'&')
        .add(b'=')
        .add(b'+');
    let base = base.trim_end_matches('/');
    let pack_enc = utf8_percent_encode(pack_id, SET).to_string();
    let rel_enc = rel_path
        .split('/')
        .map(|seg| utf8_percent_encode(seg, SET).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("{base}/v1/authoring/packs/{pack_enc}/static/{rel_enc}")
}

// ── misc ───────────────────────────────────────────────────────────────────

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}
