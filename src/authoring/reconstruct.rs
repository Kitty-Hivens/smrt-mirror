//! Reconstruct an editable `PackConfig` from a published `PackManifest` +
//! `PackSummary`. Packs onboarded through the old CLI pipeline never had their
//! config persisted under `authoring/`, so the panel can't open them. This
//! rebuilds the authoring config from what the mirror already holds, so the
//! operator can edit and rebuild the pack through the control panel.

use crate::domain::{
    AssetEntry, DeclaredAsset, DeclaredMod, ModEntry, PackConfig, PackManifest, PackMeta,
    PackSummary, Source, SourceDecl,
};

/// The manifest carries the launch facts (mc / loader / java) and the resolved
/// mods + assets; the summary carries the card metadata (name / tagline / tags
/// / featured). Together they reproduce the authoring config the build consumes.
pub fn reconstruct_config(manifest: &PackManifest, summary: &PackSummary) -> PackConfig {
    PackConfig {
        pack_id: summary.pack_id.clone(),
        display_name: summary.display_name.clone(),
        tagline: summary.tagline.clone(),
        minecraft_version: manifest.minecraft.version.clone(),
        loader: manifest.loader.clone(),
        java_major: manifest.java.major,
        version: None,
        tags: summary.tags.clone(),
        featured: summary.featured,
        mods: manifest.mods.iter().map(reconstruct_mod).collect(),
        assets: manifest.assets.iter().map(reconstruct_asset).collect(),
        auth: manifest.auth.clone(),
        pack_meta: PackMeta {
            icon_url: summary.icon_url.clone(),
            banner_url: summary.banner_url.clone(),
            gallery_urls: summary.gallery_urls.clone(),
            description_md: summary.description_md.clone(),
            tagline_i18n: summary.tagline_i18n.clone(),
            description_md_i18n: summary.description_md_i18n.clone(),
        },
        owner: summary.owner,
        tier: summary.tier,
        visibility: summary.visibility,
        fork_of: summary.fork_of.clone(),
    }
}

fn reconstruct_mod(m: &ModEntry) -> DeclaredMod {
    DeclaredMod {
        filename: m.filename.clone(),
        default_enabled: m.default_enabled,
        source: source_decl(&m.source, &m.sha1),
        display: m.display.clone(),
        slug: m.slug.clone(),
        pulled: false,
    }
}

fn reconstruct_asset(a: &AssetEntry) -> DeclaredAsset {
    DeclaredAsset {
        dest: a.dest.clone(),
        required: a.required,
        source: source_decl(&a.source, &a.sha1),
        display: a.display.clone(),
    }
}

/// A wire `Source` carries a served URL (cache / static) or Modrinth ids; the
/// authoring `SourceDecl` carries the hash / rel_path the build re-resolves
/// from. The entry's own sha1 is the cached jar's content hash, so a cache
/// source needs no URL parse; a static source recovers its rel_path from the
/// `/static/<rel>` tail.
fn source_decl(source: &Source, sha1: &str) -> SourceDecl {
    match source {
        Source::Modrinth {
            project_id,
            version_id,
        } => SourceDecl::Modrinth {
            project_id: project_id.clone(),
            version_id: version_id.clone(),
        },
        Source::CurseForge {
            project_id,
            file_id,
            ..
        } => SourceDecl::CurseForge {
            project_id: *project_id,
            file_id: *file_id,
        },
        Source::Github {
            repo, tag, asset, ..
        } => SourceDecl::Github {
            repo: repo.clone(),
            tag: tag.clone(),
            asset: asset.clone(),
        },
        Source::SmrtCache { .. } => SourceDecl::SmrtCache {
            sha1: sha1.to_string(),
        },
        Source::SmrtStatic { url } => SourceDecl::SmrtStatic {
            rel_path: rel_from_static_url(url),
        },
    }
}

/// Recover the static rel_path from a served URL `.../static/<rel>`, whose
/// segments were percent-encoded by `static_url`. Falls back to the raw tail
/// if the marker is absent.
fn rel_from_static_url(url: &str) -> String {
    let tail = url.split("/static/").nth(1).unwrap_or(url);
    tail.split('/')
        .map(|seg| {
            percent_encoding::percent_decode_str(seg)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Display, JavaSpec, LoaderSpec, MinecraftSpec, PackTier, Visibility};
    use std::collections::BTreeMap;

    fn manifest() -> PackManifest {
        PackManifest {
            schema_version: 2,
            channel: None,
            changelog: None,
            changelog_i18n: None,
            pack_id: "Industrial".into(),
            pack_version: "2026.05.30.1".into(),
            generated_at: "now".into(),
            fingerprint: None,
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
            mods: vec![
                ModEntry {
                    filename: "jei.jar".into(),
                    sha1: "a".repeat(40),
                    size_bytes: 10,
                    required: true,
                    default_enabled: true,
                    source: Source::Modrinth {
                        project_id: "P".into(),
                        version_id: "V".into(),
                    },
                    display: Some(Display {
                        name: Some("JEI".into()),
                        ..Default::default()
                    }),
                    slug: None,
                },
                ModEntry {
                    filename: "open-smrt.jar".into(),
                    sha1: "b".repeat(40),
                    size_bytes: 20,
                    required: true,
                    default_enabled: false,
                    source: Source::SmrtCache {
                        url: "https://m/v1/cache/bb/bbb.jar".into(),
                    },
                    display: None,
                    slug: None,
                },
            ],
            assets: vec![AssetEntry {
                dest: "shaderpacks/BSL (v8+).zip".into(),
                sha1: "c".repeat(40),
                size_bytes: 30,
                required: false,
                source: Source::SmrtStatic {
                    url: "https://m/v1/packs/Industrial/static/shaderpacks/BSL%20(v8%2B).zip"
                        .into(),
                },
                display: None,
            }],
        }
    }

    fn summary() -> PackSummary {
        PackSummary {
            pack_id: "Industrial".into(),
            display_name: "Industrial".into(),
            tagline: "tag".into(),
            minecraft_version: "1.12.2".into(),
            latest_pack_version: "2026.05.30.1".into(),
            tags: vec!["tech".into()],
            featured: false,
            icon_url: Some("https://m/v1/packs/Industrial/static/_nexira/icon.png".into()),
            banner_url: None,
            gallery_urls: vec!["https://m/g1.png".into()],
            description_md: Some("# Industrial".into()),
            tagline_i18n: Some(BTreeMap::from([("ru".to_string(), "тег".to_string())])),
            description_md_i18n: Some(BTreeMap::from([(
                "ru".to_string(),
                "# Индустриальный".to_string(),
            )])),
            owner: 211033194,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
            latest_built_at: None,
            latest_channel: None,
        }
    }

    #[test]
    fn reconstructs_launch_facts_and_sources() {
        let cfg = reconstruct_config(&manifest(), &summary());
        assert_eq!(cfg.minecraft_version, "1.12.2");
        assert_eq!(cfg.java_major, 8);
        assert_eq!(cfg.loader.version, "14.23.5.2922");
        assert_eq!(cfg.mods.len(), 2);
        // modrinth keeps ids; cache keeps the entry's own sha1
        match &cfg.mods[0].source {
            SourceDecl::Modrinth { project_id, .. } => assert_eq!(project_id, "P"),
            _ => panic!("expected modrinth"),
        }
        match &cfg.mods[1].source {
            SourceDecl::SmrtCache { sha1 } => assert_eq!(sha1, &"b".repeat(40)),
            _ => panic!("expected cache"),
        }
        assert!(!cfg.mods[1].default_enabled);
        // static asset recovers a percent-decoded rel_path
        match &cfg.assets[0].source {
            SourceDecl::SmrtStatic { rel_path } => {
                assert_eq!(rel_path, "shaderpacks/BSL (v8+).zip")
            }
            _ => panic!("expected static"),
        }
        // pack-card metadata is recovered from the summary so revert keeps the card
        assert_eq!(
            cfg.pack_meta.icon_url.as_deref(),
            Some("https://m/v1/packs/Industrial/static/_nexira/icon.png")
        );
        assert_eq!(
            cfg.pack_meta.gallery_urls,
            vec!["https://m/g1.png".to_string()]
        );
        assert_eq!(
            cfg.pack_meta.description_md.as_deref(),
            Some("# Industrial")
        );
        // including what the card says in every other language it was written
        // in: a revert that recovered only the untagged copy would quietly
        // delete the translations, and the next build would ship the card
        // half-translated
        assert_eq!(
            cfg.pack_meta
                .tagline_i18n
                .as_ref()
                .and_then(|m| m.get("ru"))
                .map(String::as_str),
            Some("тег")
        );
        assert_eq!(
            cfg.pack_meta
                .description_md_i18n
                .as_ref()
                .and_then(|m| m.get("ru"))
                .map(String::as_str),
            Some("# Индустриальный")
        );
    }
}
