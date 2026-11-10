//! Which of a pack's unmet Modrinth dependencies apply on the loader it runs.
//!
//! A Modrinth version carries one dependency list for every loader it is
//! published for. More Enchantment Info 0.4.2 is one version for Fabric, Forge
//! and NeoForge, and lists fabric-language-kotlin (Fabric only) beside
//! KotlinLangForge (Forge and NeoForge only). Read literally, a NeoForge pack
//! misses a Fabric library it never loads, and the gate refuses to publish it.
//!
//! Such a list means "required where it exists": each dependency is answered by
//! a version of that project for the loader and game version in play, and one
//! with no such version does not apply there. Authors publish multi-loader
//! versions on exactly that reading. A dependency pinned to an exact version
//! names the build it wants regardless of loader, so a pinned one always
//! applies.
//!
//! The loader is the one the requirer runs on: the pack's own for a native mod,
//! and the loaders a connector carries it on for a bridged one. A Fabric mod
//! riding Sinytra Connector keeps its Fabric API dependency, which the
//! resolver then answers with Forgified Fabric API like any other present mod.

use super::modrinth::Modrinth;
use super::resolve::{LoaderMismatch, MissingDep, ResolveReport};
use crate::domain::PackConfig;
use crate::registry::{Registry, queries};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Version loaders that suit every mod loader: a data pack loads through any
/// of them, and `minecraft` marks content that needs no
/// loader at all.
const UNIVERSAL_LOADERS: [&str; 2] = ["minecraft", "datapack"];

/// Move every unmet Modrinth dependency that has no build for its requirer's
/// loader at the pack's game version out of `missing` and into
/// `not_on_loader`.
///
/// Never fails the caller. A project Modrinth does not answer for stays
/// missing: an outage is not evidence that the dependency does not apply.
pub async fn scope_modrinth_deps(
    cfg: &PackConfig,
    report: &mut ResolveReport,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
) {
    let projects: HashSet<String> = report
        .missing
        .iter()
        .filter_map(unpinned_project)
        .map(str::to_string)
        .collect();
    if projects.is_empty() {
        return;
    }
    let chain = pack_loaders(cfg, registry).await;
    let mut published: HashMap<String, HashSet<String>> = HashMap::new();
    for project in projects {
        match modrinth
            .project_versions(&project, Some(&cfg.minecraft_version))
            .await
        {
            Ok(versions) => {
                let loaders = versions
                    .into_iter()
                    .flat_map(|v| v.loaders)
                    .map(|l| l.to_ascii_lowercase())
                    .collect();
                published.insert(project, loaders);
            }
            Err(e) => tracing::warn!(
                project = %project,
                error = %format!("{e:#}"),
                "could not read which loaders a dependency is published for; it stays missing"
            ),
        }
    }
    let missing = std::mem::take(&mut report.missing);
    let (kept, skipped): (Vec<MissingDep>, Vec<MissingDep>) = missing
        .into_iter()
        .partition(|m| applies(m, &published, &chain, &report.loader_bridged));
    report.missing = kept;
    report.not_on_loader = skipped;
}

/// The Modrinth project an unmet dependency names, when Modrinth declared it
/// without pinning a version. A pinned dependency carries its version id in
/// the window slot.
fn unpinned_project(m: &MissingDep) -> Option<&str> {
    if m.source != "modrinth" || m.version_range.is_some() {
        return None;
    }
    let bare = m.target.split('@').next().unwrap_or(&m.target);
    bare.strip_prefix("modrinth:").filter(|p| !p.is_empty())
}

/// Every loader whose artifacts the pack runs natively: its own and whatever it
/// inherits from. Falls back to the pack's own loader when the registry cannot
/// say.
async fn pack_loaders(cfg: &PackConfig, registry: &Arc<Registry>) -> HashSet<String> {
    let loader = cfg.loader.name.trim().to_ascii_lowercase();
    let asked = loader.clone();
    let mut chain = registry
        .read(move |c| queries::loader_chain(c, &asked))
        .await
        .unwrap_or_default();
    chain.insert(loader);
    chain
}

/// Whether an unmet dependency applies to at least one of the mods requiring
/// it. Anything this pass cannot judge applies, so it stays a finding.
fn applies(
    m: &MissingDep,
    published: &HashMap<String, HashSet<String>>,
    pack_loaders: &HashSet<String>,
    bridged: &[LoaderMismatch],
) -> bool {
    let Some(project) = unpinned_project(m) else {
        return true;
    };
    let Some(loaders) = published.get(project) else {
        return true;
    };
    if loaders
        .iter()
        .any(|l| UNIVERSAL_LOADERS.contains(&l.as_str()))
    {
        return true;
    }
    m.needed_by.iter().any(
        |requirer| match bridged.iter().find(|b| &b.filename == requirer) {
            Some(b) => b
                .artifact_loaders
                .iter()
                .any(|l| loaders.contains(&l.to_ascii_lowercase())),
            None => pack_loaders.iter().any(|l| loaders.contains(l)),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(target: &str, needed_by: &[&str], pinned: Option<&str>) -> MissingDep {
        MissingDep {
            target: target.into(),
            needed_by: needed_by.iter().map(|s| s.to_string()).collect(),
            version_range: pinned.map(str::to_string),
            source: "modrinth".into(),
            reason: None,
        }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn published(rows: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
        rows.iter().map(|(p, l)| (p.to_string(), set(l))).collect()
    }

    // More Enchantment Info 0.4.2 and Preloading Tricks 3.7.3 on NeoForge: the
    // Fabric libraries they list have no NeoForge build, so they do not apply,
    // while KotlinLangForge has one and stays.
    #[test]
    fn a_dependency_with_no_build_for_the_pack_loader_does_not_apply() {
        let mr = published(&[
            ("Ha28R6CL", &["fabric"]),
            ("P7dR8mSH", &["fabric"]),
            ("1vrSzlao", &["forge", "neoforge"]),
        ]);
        let neo = set(&["neoforge"]);
        let flk = dep("modrinth:Ha28R6CL", &["more-enchantment-info.jar"], None);
        let api = dep("modrinth:P7dR8mSH", &["preloading-tricks.jar"], None);
        let klf = dep("modrinth:1vrSzlao", &["more-enchantment-info.jar"], None);
        assert!(!applies(&flk, &mr, &neo, &[]));
        assert!(!applies(&api, &mr, &neo, &[]));
        assert!(applies(&klf, &mr, &neo, &[]));
    }

    // A Fabric mod on a NeoForge pack through a connector runs as a Fabric mod,
    // so its Fabric API dependency is real there.
    #[test]
    fn a_bridged_requirer_is_judged_on_the_loader_it_is_carried_on() {
        let mr = published(&[("P7dR8mSH", &["fabric"])]);
        let bridged = vec![LoaderMismatch {
            filename: "fabric-mod.jar".into(),
            artifact_loaders: vec!["fabric".into()],
            pack_loader: "neoforge".into(),
            bridged_by: Some("connector.jar".into()),
        }];
        let api = dep("modrinth:P7dR8mSH", &["fabric-mod.jar"], None);
        assert!(applies(&api, &mr, &set(&["neoforge"]), &bridged));
        // and one native requirer beside it does not take that away
        let both = dep(
            "modrinth:P7dR8mSH",
            &["preloading-tricks.jar", "fabric-mod.jar"],
            None,
        );
        assert!(applies(&both, &mr, &set(&["neoforge"]), &bridged));
    }

    // What the pass cannot judge stays a finding: a pinned version (it names
    // its build whatever the loader), a project Modrinth did not answer for,
    // an edge that did not come from Modrinth, and universal content.
    #[test]
    fn what_cannot_be_judged_still_applies() {
        let mr = published(&[("P7dR8mSH", &["fabric"]), ("DATAPACK", &["datapack"])]);
        let neo = set(&["neoforge"]);
        assert!(applies(
            &dep("modrinth:P7dR8mSH", &["a.jar"], Some("VERSIONID")),
            &mr,
            &neo,
            &[]
        ));
        assert!(applies(
            &dep("modrinth:UNREAD", &["a.jar"], None),
            &mr,
            &neo,
            &[]
        ));
        let mut declared = dep("modrinth:P7dR8mSH", &["a.jar"], None);
        declared.source = "jar-meta".into();
        assert!(applies(&declared, &mr, &neo, &[]));
        assert!(applies(
            &dep("modrinth:DATAPACK", &["a.jar"], None),
            &mr,
            &neo,
            &[]
        ));
    }
}
