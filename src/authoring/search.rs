//! One search for "which mod do I add", over both places a mod can come from.
//!
//! Adding a mod used to start with a question nobody can answer yet: from the
//! mirror, or from Modrinth? Provenance is the mirror's problem -- it holds the
//! bytes or it does not -- and asking the curator to decide it first meant
//! knowing whether the mirror carries something they have not found (#101).
//!
//! So both are searched and the results are merged. What the mirror has
//! harvested it knows exactly, down to each artifact's loader targets; an
//! unfamiliar Modrinth project it knows only as well as Modrinth's own facets.
//! Both are answered, and each hit says which it is, rather than the weaker
//! knowledge being hidden or the stronger being thrown away.

use super::modrinth::Modrinth;
use crate::registry::{Registry, queries};
use crate::storage::Storage;
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use ts_rs::TS;

/// How a mod sits with the pack's loader. Deliberately not a boolean: the
/// registry models four different answers and flattening them would either hide
/// working mods or promise ones that cannot load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "snake_case")]
pub enum LoaderFit {
    /// Targets the pack's loader, or one it inherits from (a Forge jar in a
    /// Cleanroom pack), or declares itself loader-agnostic.
    Native,
    /// A foreign loader carried by a bridge the pack already ships -- Fabric
    /// mods behind a connector that is in the pack. They load; they also all
    /// leave together if that one mod does.
    Bridged,
    /// A foreign loader a known bridge could carry, but the pack does not ship
    /// the bridge. Not usable as the pack stands, and one addition away from
    /// being usable -- a different thing to say than "will not load".
    Bridgeable,
    /// Neither: it would not load in this pack as it stands.
    Foreign,
    /// Nothing said which loaders it targets, so nothing is claimed.
    Unknown,
}

/// One candidate to add, from either side.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ModHit {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// The Modrinth project, when the mod is catalogued there -- what a pinned
    /// declaration needs, and what the project icon is keyed by.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modrinth_project_id: Option<String>,
    /// The registry's own id, when the mirror has harvested it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional)]
    pub mod_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon_sha1: Option<String>,
    /// True when the mirror holds bytes for this mod: the pack can declare a
    /// cache source and the launcher never leaves the mirror for it.
    pub mirrored: bool,
    pub fit: LoaderFit,
    /// Which loader the bridge carries, when `fit` is bridged -- the fact worth
    /// stating, since removing that one mod takes every mod riding it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bridged_from: Option<String>,
    pub loaders: Vec<String>,
    pub mc_versions: Vec<String>,
}

/// The pack a search is judged against. Grouped because it is one thing -- the
/// pack being filled -- and because passing its three facts separately made the
/// signature read as a pile of options rather than a question about a pack.
pub struct PackContext<'a> {
    pub mc: Option<&'a str>,
    pub loader: Option<&'a str>,
    /// Modrinth project ids the pack already declares. A bridge among them is
    /// what separates "rides a connector this pack has" from "would need one".
    pub present_projects: &'a HashSet<String>,
}

/// Search both sides for `q`, judged against the pack the curator is filling.
///
/// `loader` and `mc` describe the pack, and neither filters: a mod that needs a
/// bridge still loads, and hiding it would be a lie. They rank instead -- native
/// first, then bridged, then unjudgeable, then foreign -- and within a tier what
/// the mirror already holds comes first, because that is the candidate with no
/// download and no upstream dependency at install time.
pub async fn search_mods(
    q: &str,
    pack: &PackContext<'_>,
    // Where the bytes would be: the registry knows an artifact exists, storage
    // knows whether the mirror holds it. Asked about the hits rather than for
    // its whole listing -- a search answers thirty rows, and reading every
    // directory in the cache to flag them is a cost that grows with the mirror
    // while the answer does not.
    storage: &Storage,
    limit: usize,
    registry: &Arc<Registry>,
    modrinth: &Modrinth,
) -> Result<Vec<ModHit>> {
    // Everything the registry has to say, in one hop off the runtime: the
    // loader family, the bridge table, the matching mods, and -- scoped to just
    // those mods -- which artifacts exist for them. Four reads that used to run
    // on the caller's worker, on a path a curator triggers with every search
    // box keystroke.
    let (chain, bridges, registry_hits, sha1s) = {
        let (q, mc, loader) = (
            q.to_string(),
            pack.mc.map(str::to_string),
            pack.loader.map(str::to_string),
        );
        registry
            .read(move |c| {
                let chain = match &loader {
                    Some(l) => queries::loader_chain(c, l)?,
                    None => Default::default(),
                };
                let bridges = queries::loader_bridges(c)?;
                // Not filtered by loader: the fit is computed below, and a
                // foreign-loader hit is still worth showing as foreign.
                let hits = queries::list_mods(c, Some(&q), None, mc.as_deref(), None, None)?;
                let ids: Vec<i64> = hits.iter().map(|m| m.mod_id).collect();
                let sha1s = queries::sha1s_for_mods(c, &ids)?;
                Ok((chain, bridges, hits, sha1s))
            })
            .await?
    };

    let cached: HashSet<String> = storage
        .cached_among(sha1s.values().flatten().cloned())
        .await;

    // Every known bridge, and the subset of them this pack already ships.
    let carried_by_pack: HashSet<String> = bridges
        .iter()
        .filter(|(project, _)| pack.present_projects.contains(*project))
        .map(|(_, loader)| loader.to_lowercase())
        .collect();
    let carried_by_any: HashSet<String> = bridges.values().map(|l| l.to_lowercase()).collect();

    // Modrinth is best-effort: an outage narrows the answer to what the mirror
    // knows rather than failing the search, which is the same posture the build
    // takes (#57).
    let upstream = match modrinth.search(q, pack.mc, "mod").await {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = %e, "modrinth search failed; answering from the registry alone");
            Vec::new()
        }
    };

    let mut out: Vec<ModHit> = Vec::new();
    let mut by_project: HashMap<String, usize> = HashMap::new();

    for m in registry_hits {
        let fit = fit_for(&m.loaders, &chain, &carried_by_pack, &carried_by_any);
        let mirrored = sha1s
            .get(&m.mod_id)
            .is_some_and(|hs| hs.iter().any(|h| cached.contains(h)));
        let hit = ModHit {
            name: m.name,
            slug: m.slug,
            author: m.author,
            description: None,
            modrinth_project_id: m.modrinth_project_id.clone(),
            mod_id: Some(m.mod_id),
            icon_url: None,
            icon_sha1: m.icon_sha1.clone(),
            mirrored,
            fit: fit.0,
            bridged_from: fit.1,
            loaders: m.loaders,
            mc_versions: m.mc_versions,
        };
        if let Some(p) = hit.modrinth_project_id.clone() {
            by_project.insert(p, out.len());
        }
        out.push(hit);
    }

    for h in upstream {
        // A project the mirror already knows is one row, not two: the registry's
        // knowledge is the better half, and Modrinth fills in what it lacks.
        if let Some(&i) = by_project.get(&h.project_id) {
            let row = &mut out[i];
            if row.description.is_none() && !h.description.is_empty() {
                row.description = Some(h.description);
            }
            if row.icon_url.is_none() {
                row.icon_url = h.icon_url;
            }
            if row.author.is_none() && !h.author.is_empty() {
                row.author = Some(h.author);
            }
            continue;
        }
        let loaders = loaders_from_categories(&h.categories);
        let fit = fit_for(&loaders, &chain, &carried_by_pack, &carried_by_any);
        out.push(ModHit {
            name: h.title,
            slug: Some(h.slug),
            author: (!h.author.is_empty()).then_some(h.author),
            description: (!h.description.is_empty()).then_some(h.description),
            modrinth_project_id: Some(h.project_id),
            mod_id: None,
            icon_url: h.icon_url,
            icon_sha1: None,
            mirrored: false,
            fit: fit.0,
            bridged_from: fit.1,
            loaders,
            mc_versions: h.versions,
        });
    }

    out.sort_by(|a, b| {
        tier(a.fit)
            .cmp(&tier(b.fit))
            .then(b.mirrored.cmp(&a.mirrored))
            .then(b.mod_id.is_some().cmp(&a.mod_id.is_some()))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.truncate(limit);
    Ok(out)
}

/// Rank order of the fit tiers. Foreign is last rather than absent: the curator
/// searched for it, and "this one will not load in this pack" is an answer.
fn tier(f: LoaderFit) -> u8 {
    match f {
        LoaderFit::Native => 0,
        LoaderFit::Bridged => 1,
        LoaderFit::Unknown => 2,
        LoaderFit::Bridgeable => 3,
        LoaderFit::Foreign => 4,
    }
}

/// Judge a mod's loader targets against the pack's loader family, falling back
/// to the bridge table for a foreign loader something can carry.
fn fit_for(
    loaders: &[String],
    chain: &HashSet<String>,
    carried_by_pack: &HashSet<String>,
    carried_by_any: &HashSet<String>,
) -> (LoaderFit, Option<String>) {
    if chain.is_empty() || loaders.is_empty() {
        return (LoaderFit::Unknown, None);
    }
    if loaders
        .iter()
        .any(|l| l == "any" || chain.contains(&l.to_lowercase()))
    {
        return (LoaderFit::Native, None);
    }
    for l in loaders.iter().map(|l| l.to_lowercase()) {
        if carried_by_pack.contains(&l) {
            return (LoaderFit::Bridged, Some(l));
        }
    }
    for l in loaders.iter().map(|l| l.to_lowercase()) {
        if carried_by_any.contains(&l) {
            return (LoaderFit::Bridgeable, Some(l));
        }
    }
    (LoaderFit::Foreign, None)
}

/// Modrinth files its loaders among the project's categories, so the loaders are
/// whichever categories name a loader the registry knows. Anything else in that
/// list is a genre tag and is ignored.
fn loaders_from_categories(categories: &[String]) -> Vec<String> {
    const KNOWN: [&str; 6] = ["forge", "neoforge", "fabric", "quilt", "cleanroom", "rift"];
    categories
        .iter()
        .map(|c| c.to_lowercase())
        .filter(|c| KNOWN.contains(&c.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // The rule the whole feature turns on: a loader is eligibility, not equality.
    // Filtering on the literal loader would hide a Forge jar from a Cleanroom
    // pack (it inherits) and hide Fabric mods a connector in the pack already
    // carries (they load) -- and would flatten "needs a connector added" into
    // "will not load", which is a different sentence.
    #[test]
    fn loader_fit_reads_the_family_the_bridge_and_the_pack() {
        let cleanroom = set(&["cleanroom", "forge"]); // as loader_chain resolves it
        let in_pack = set(&["fabric"]); // the pack ships a connector
        let anywhere = set(&["fabric"]);
        let none = HashSet::new();

        // inherited: a forge jar is native to a cleanroom pack
        assert_eq!(
            fit_for(&["forge".into()], &cleanroom, &none, &anywhere).0,
            LoaderFit::Native
        );
        // loader-agnostic
        assert_eq!(
            fit_for(&["any".into()], &cleanroom, &none, &anywhere).0,
            LoaderFit::Native
        );
        // carried by a bridge the pack has
        let (fit, from) = fit_for(&["fabric".into()], &cleanroom, &in_pack, &anywhere);
        assert_eq!(fit, LoaderFit::Bridged);
        assert_eq!(from.as_deref(), Some("fabric"));
        // a bridge exists, but this pack does not ship it
        assert_eq!(
            fit_for(&["fabric".into()], &cleanroom, &none, &anywhere).0,
            LoaderFit::Bridgeable
        );
        // nothing carries it
        assert_eq!(
            fit_for(&["rift".into()], &cleanroom, &none, &anywhere).0,
            LoaderFit::Foreign
        );
        // nothing said: claim nothing
        assert_eq!(
            fit_for(&[], &cleanroom, &none, &anywhere).0,
            LoaderFit::Unknown
        );
        assert_eq!(
            fit_for(&["forge".into()], &HashSet::new(), &none, &anywhere).0,
            LoaderFit::Unknown,
            "no pack loader to judge against"
        );
    }

    // Ranking order, and why foreign is last rather than absent: the curator
    // searched for it, and "this will not load here" is an answer.
    #[test]
    fn tiers_rank_usable_first_and_never_drop_a_hit() {
        let mut order = [
            LoaderFit::Foreign,
            LoaderFit::Unknown,
            LoaderFit::Native,
            LoaderFit::Bridgeable,
            LoaderFit::Bridged,
        ];
        order.sort_by_key(|f| tier(*f));
        assert_eq!(
            order,
            [
                LoaderFit::Native,
                LoaderFit::Bridged,
                LoaderFit::Unknown,
                LoaderFit::Bridgeable,
                LoaderFit::Foreign,
            ]
        );
    }

    // Modrinth files loaders among the project's categories; everything else in
    // that list is a genre tag and must not be read as a loader.
    #[test]
    fn loaders_are_picked_out_of_modrinth_categories() {
        let cats = [
            "forge".to_string(),
            "fabric".to_string(),
            "technology".to_string(),
            "Storage".to_string(),
        ];
        assert_eq!(loaders_from_categories(&cats), vec!["forge", "fabric"]);
    }
}
