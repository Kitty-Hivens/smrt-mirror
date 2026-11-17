//! The bootstrap pass: read an instance archive, stage its mods into the cache +
//! extras into the pack's static area, and return a starter `PackConfig`.
//! Modrinth matches become modrinth sources; the rest become smrt_cache /
//! smrt_static. The caller persists the returned config.

use super::archive::{extract_extra_assets, extract_mods};
use super::modrinth::Modrinth;
use super::sources::{write_to_cache, write_to_static};
use crate::domain::{DeclaredAsset, DeclaredMod, LoaderSpec, PackConfig, SourceDecl};
use crate::storage::is_safe_id;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use tracing::info;

pub struct BootstrapArgs {
    pub pack_id: String,
    pub display_name: String,
    pub tagline: String,
    pub minecraft_version: String,
    pub loader: LoaderSpec,
    pub java_major: u32,
    /// Storage root: extracted mod jars land in `{storage}/cache/`, extras
    /// land in `{storage}/packs/{pack_id}/static/`.
    pub storage: PathBuf,
    /// Who the new pack belongs to. Like every new pack it is their personal
    /// draft until an admin makes it official.
    pub owner: i64,
}

pub async fn bootstrap(args: BootstrapArgs, archive: Vec<u8>) -> Result<PackConfig> {
    info!(bytes = archive.len(), "loaded instance archive");

    // The pack id becomes a path segment under storage/packs/<id>/ before the
    // config (the usual id guard) is ever written, so validate it here too: a
    // traversal id would stage archive files outside the storage tree.
    if !is_safe_id(&args.pack_id) {
        bail!("invalid pack id {:?}", args.pack_id);
    }

    // Unzipping the whole archive is synchronous, CPU-heavy work; run it on
    // the blocking pool so it never stalls an async runtime worker (the public
    // /v1 read API shares them).
    let (mods, extras) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mods = extract_mods(&archive)?;
        let extras = extract_extra_assets(&archive)?;
        Ok((mods, extras))
    })
    .await
    .context("archive extraction task")??;

    info!(count = mods.len(), "discovered mods in archive");
    if mods.is_empty() {
        bail!("no mods/*.jar in archive -- wrong archive layout?");
    }
    info!(count = extras.len(), "discovered extras files");

    let modrinth = Modrinth::new()?;
    let sha1s: Vec<String> = mods.iter().map(|m| m.sha1.clone()).collect();
    let hits = modrinth.version_files_by_sha1(&sha1s).await?;
    info!(matched = hits.len(), total = sha1s.len(), "modrinth lookup");

    // Staging is per-file std::fs writes -- also synchronous. Classify each mod
    // against the Modrinth hits and write the unmatched jars + extras on the
    // blocking pool.
    let pack_id = args.pack_id.clone();
    let mc = args.minecraft_version.clone();
    let loader_name = args.loader.name.clone();
    let storage = args.storage.clone();
    let (declared_mods, declared_assets) = tokio::task::spawn_blocking(move || -> Result<_> {
        let static_dir = storage.join("packs").join(&pack_id).join("static");

        let mut declared_mods = Vec::with_capacity(mods.len());
        for m in &mods {
            let decl = if let Some(hit) = hits.get(&m.sha1) {
                let mc_ok = hit.game_versions.iter().any(|v| v == &mc);
                let loader_ok = hit
                    .loaders
                    .iter()
                    .any(|l| l.eq_ignore_ascii_case(&loader_name));
                if mc_ok && loader_ok {
                    DeclaredMod {
                        filename: m.filename.clone(),
                        default_enabled: true,
                        source: SourceDecl::Modrinth {
                            project_id: hit.project_id.clone(),
                            version_id: hit.id.clone(),
                        },
                        display: None,
                        slug: None,
                        pulled: false,
                    }
                } else {
                    write_to_cache(&storage, &m.sha1, &m.bytes)?;
                    DeclaredMod {
                        filename: m.filename.clone(),
                        default_enabled: true,
                        source: SourceDecl::SmrtCache {
                            sha1: m.sha1.clone(),
                        },
                        display: None,
                        slug: None,
                        pulled: false,
                    }
                }
            } else {
                write_to_cache(&storage, &m.sha1, &m.bytes)?;
                DeclaredMod {
                    filename: m.filename.clone(),
                    default_enabled: true,
                    source: SourceDecl::SmrtCache {
                        sha1: m.sha1.clone(),
                    },
                    display: None,
                    slug: None,
                    pulled: false,
                }
            };
            declared_mods.push(decl);
        }
        declared_mods.sort_by(|a, b| a.filename.cmp(&b.filename));

        let mut declared_assets = Vec::with_capacity(extras.len());
        for a in &extras {
            write_to_static(&static_dir, &a.rel_path, &a.bytes)?;
            declared_assets.push(DeclaredAsset {
                dest: a.rel_path.clone(),
                required: true,
                source: SourceDecl::SmrtStatic {
                    rel_path: a.rel_path.clone(),
                },
                display: None,
            });
        }
        declared_assets.sort_by(|a, b| a.dest.cmp(&b.dest));

        Ok((declared_mods, declared_assets))
    })
    .await
    .context("archive staging task")??;

    Ok(PackConfig {
        pack_id: args.pack_id,
        display_name: args.display_name,
        tagline: args.tagline,
        minecraft_version: args.minecraft_version,
        loader: args.loader,
        java_major: args.java_major,
        version: None,
        auth: None,
        tags: Vec::new(),
        featured: false,
        mods: declared_mods,
        assets: declared_assets,
        pack_meta: Default::default(),
        owner: args.owner,
        tier: crate::domain::PackTier::Community,
        visibility: crate::domain::Visibility::Draft,
        fork_of: None,
    })
}
