//! Relational mod-identity registry (embedded SQLite). The mirror's index of
//! mods by identity (not filename): which packs use a mod, all versions of a
//! mod, orphan jars, loader-target eligibility, and sourced dep/conflict facts.
//!
//! Jar blobs stay content-addressed on the FS cache; this holds metadata +
//! relations. Every fact row carries a `source` so harvested rows (rebuildable
//! from jars + Modrinth) stay distinct from authored rows (manual moderation,
//! Phase 2) and a re-harvest never clobbers the authored ones.

pub(crate) mod authored;
pub mod classify;
mod db;
mod migrations;
pub mod model;
pub mod queries;
pub(crate) mod semver;
pub(crate) mod upsert;

pub use db::Registry;

#[cfg(test)]
mod tests {

    // #123: the unidentified bucket was a hash and a file size, so naming a jar
    // meant downloading and opening it -- which is why it only ever grew. The
    // harvest already has the file open; this is that readout surviving even
    // when no identity could be derived.
    // #145 stores two halves: what an artifact's required mixins must resolve,
    // and what each artifact provides to resolve it with. Both are only useful
    // if "cannot answer" stays distinguishable from "no".
    #[test]
    fn a_digest_answers_membership_and_absence_says_so() {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let m = upsert::upsert_mod_by_alias(c, &[("modid", "sodium")], NOW)?;
            let v = upsert::upsert_mod_version(
                c,
                m,
                "0.8.12",
                &["neoforge"],
                &"a".repeat(40),
                1,
                None,
                None,
                NOW,
            )?;

            // never read: the answer is "unknown", not "missing"
            assert_eq!(
                queries::artifact_has_class(c, v, "net/caffeinemc/Anything")?,
                None
            );

            upsert::set_class_digest(
                c,
                v,
                &[
                    "net/caffeinemc/mods/sodium/client/render/SodiumWorldRenderer".to_string(),
                    "net/caffeinemc/mods/sodium/client/SodiumClientMod".to_string(),
                ],
            )?;
            assert_eq!(
                queries::artifact_has_class(
                    c,
                    v,
                    "net/caffeinemc/mods/sodium/client/render/SodiumWorldRenderer"
                )?,
                Some(true)
            );
            // the class the new Sodium moved -- the whole point of the check
            assert_eq!(
                queries::artifact_has_class(
                    c,
                    v,
                    "net/caffeinemc/mods/sodium/client/gui/SodiumGameOptions"
                )?,
                Some(false)
            );

            upsert::set_mixin_needs(
                c,
                v,
                &[(
                    "sable.mixins.json".to_string(),
                    "net/caffeinemc/mods/sodium/client/gui/SodiumGameOptions".to_string(),
                )],
            )?;
            assert_eq!(
                queries::mixin_needs(c, v)?,
                vec![(
                    "sable.mixins.json".to_string(),
                    "net/caffeinemc/mods/sodium/client/gui/SodiumGameOptions".to_string()
                )]
            );
            // a re-harvest states the set again rather than adding to it
            upsert::set_mixin_needs(c, v, &[])?;
            assert!(queries::mixin_needs(c, v)?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    /// The negative is the point. "Asked, and CurseForge does not know it" is
    /// the answer that narrows a self-hosted jar down to a forgery, and it is
    /// indistinguishable from "never asked" unless it is written down.
    #[test]
    fn a_curseforge_miss_is_recorded_and_not_asked_again_while_a_hit_is_final() {
        use crate::authoring::curseforge::Match;
        let r = Registry::open_in_memory().unwrap();
        let hit = "a".repeat(40);
        let miss = "b".repeat(40);
        let never = "c".repeat(40);
        let found = Match {
            project_id: 243_121,
            file_id: 2_924_091,
            display_name: "Quark-r1.6-179.jar".into(),
            file_name: "Quark-r1.6-179.jar".into(),
            download_url: Some("https://example.invalid/quark.jar".into()),
            game_versions: vec!["1.12.2".into()],
        };
        r.with_conn_mut(|c| {
            // jar_read is what the leg is driven from: a jar nobody could
            // identify has a row here and none in mod_version, and it is the
            // one most worth asking CurseForge about.
            for sha in [&hit, &miss, &never] {
                upsert::set_jar_read(
                    c,
                    sha,
                    &upsert::JarRead {
                        modid: None,
                        name: None,
                        version: None,
                        loaders: &[],
                        mc: &[],
                        filename: None,
                    },
                )?;
            }
            upsert::set_curseforge_file(
                c,
                &hit,
                2_910_837_341,
                Some(&found),
                "2026-01-01T00:00:00Z",
            )?;
            upsert::set_curseforge_file(c, &miss, 1, None, "2026-01-01T00:00:00Z")?;
            Ok(())
        })
        .unwrap();

        r.with_conn(|c| {
            let h = queries::curseforge_identity(c, &hit)?.expect("the hit was recorded");
            assert_eq!(h.project_id, Some(243_121));
            assert!(h.distributable, "a download url means it may be served");

            let m = queries::curseforge_identity(c, &miss)?.expect("the miss was recorded too");
            assert_eq!(
                m.project_id, None,
                "recorded, and recorded as knowing nothing"
            );
            assert!(!m.distributable);

            // Later than both rows: the miss is due another attempt, the hit
            // never is, and the jar nobody asked about is always due one.
            let awaiting = queries::shas_awaiting_curseforge(c, "2026-06-01T00:00:00Z")?;
            assert!(awaiting.contains(&miss), "a miss can become a hit later");
            assert!(awaiting.contains(&never), "never asked is always due");
            assert!(
                !awaiting.contains(&hit),
                "a match is about bytes that cannot change, so it is never re-asked"
            );

            // Earlier than the rows: the miss is not yet stale.
            let fresh = queries::shas_awaiting_curseforge(c, "2025-01-01T00:00:00Z")?;
            assert!(!fresh.contains(&miss));
            assert!(fresh.contains(&never));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_jar_the_harvest_could_not_identify_still_says_what_it_is() {
        let r = Registry::open_in_memory().unwrap();
        let sha = "d".repeat(40);
        r.with_conn_mut(|c| {
            upsert::set_jar_read(
                c,
                &sha,
                &upsert::JarRead {
                    modid: Some("railcraft"),
                    name: Some("Railcraft"),
                    version: Some("12.0.0"),
                    loaders: &["forge".to_string()],
                    mc: &["1.12.2".to_string()],
                    filename: Some("railcraft-12.0.0.jar"),
                },
            )?;
            Ok(())
        })
        .unwrap();

        let rows = r.with_conn(queries::jar_readouts).unwrap();
        let row = rows
            .get(&sha)
            .expect("the readout is keyed by content hash");
        assert_eq!(row.modid.as_deref(), Some("railcraft"));
        assert_eq!(row.version.as_deref(), Some("12.0.0"));
        assert_eq!(row.loaders.as_deref(), Some("forge"));
        assert_eq!(row.kind, None, "no classification is not a missing readout");

        // A later harvest that reads less must not erase what an earlier one
        // read: a jar whose metadata is unreadable this time is the same jar.
        r.with_conn_mut(|c| {
            upsert::set_jar_read(
                c,
                &sha,
                &upsert::JarRead {
                    modid: None,
                    name: None,
                    version: None,
                    loaders: &[],
                    mc: &[],
                    filename: None,
                },
            )?;
            Ok(())
        })
        .unwrap();
        let rows = r.with_conn(queries::jar_readouts).unwrap();
        assert_eq!(rows[&sha].modid.as_deref(), Some("railcraft"));
    }
    use super::model::{RelKind, Severity, Source};
    use super::{Registry, queries, upsert};

    const NOW: &str = "2026-06-06T00:00:00Z";

    // A registry with two forge mods (one referenced by a build, one orphan),
    // an `any` tweaker, a cleanroom-only artifact, a forge+fabric multi-loader
    // jar, a pack build, and a relation.
    fn fixture() -> Registry {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            // appleskin: carries both a modid and a Modrinth project id
            let apple = upsert::upsert_mod_by_alias(
                c,
                &[("modid", "appleskin"), ("modrinth", "EsAfb37o")],
                NOW,
            )?;
            let apple_v = upsert::upsert_mod_version(
                c,
                apple,
                "2.5.1",
                &["forge"],
                "sha_apple",
                1000,
                Some("appleskin.jar"),
                None,
                NOW,
            )?;
            // jei: forge, modid only
            let jei = upsert::upsert_mod_by_alias(c, &[("modid", "jei")], NOW)?;
            let jei_v = upsert::upsert_mod_version(
                c,
                jei,
                "4.16",
                &["forge"],
                "sha_jei",
                2000,
                Some("jei.jar"),
                None,
                NOW,
            )?;
            // a loader-agnostic tweaker
            let tw = upsert::upsert_mod_by_alias(c, &[("modid", "tweak")], NOW)?;
            let tw_v = upsert::upsert_mod_version(
                c,
                tw,
                "1.0",
                &["any"],
                "sha_tweak",
                300,
                Some("tweak.jar"),
                None,
                NOW,
            )?;
            // a cleanroom-only artifact (not in any build)
            let cr = upsert::upsert_mod_by_alias(c, &[("modid", "crmod")], NOW)?;
            upsert::upsert_mod_version(
                c,
                cr,
                "1.0",
                &["cleanroom"],
                "sha_cr",
                400,
                Some("cr.jar"),
                None,
                NOW,
            )?;
            // a single jar published for two loaders at once (Modrinth set)
            let multi = upsert::upsert_mod_by_alias(c, &[("modid", "multimod")], NOW)?;
            upsert::upsert_mod_version(
                c,
                multi,
                "3.0",
                &["forge", "fabric"],
                "sha_multi",
                600,
                Some("multi.jar"),
                None,
                NOW,
            )?;
            // an orphan forge artifact (cached, no build references it)
            let orph = upsert::upsert_mod_by_alias(c, &[("modid", "orphanmod")], NOW)?;
            upsert::upsert_mod_version(
                c,
                orph,
                "0.1",
                &["forge"],
                "sha_orphan",
                50,
                Some("orphan.jar"),
                None,
                NOW,
            )?;
            // a pack build shipping appleskin + jei + tweak
            upsert::upsert_pack(c, "Industrial", NOW)?;
            let build = upsert::upsert_pack_build(
                c,
                "Industrial",
                "2026.06.06",
                "1.12.2",
                Some("forge"),
                Some("14.23"),
                Some(8),
                Some("fp_industrial"),
                true,
                NOW,
            )?;
            upsert::link_build_mod(c, build, apple_v, "appleskin.jar", true, true)?;
            upsert::link_build_mod(c, build, jei_v, "jei.jar", true, true)?;
            upsert::link_build_mod(c, build, tw_v, "tweak.jar", false, true)?;
            // jei requires appleskin (jar-meta)
            upsert::upsert_relation(
                c,
                jei,
                None,
                "appleskin",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        r
    }

    #[test]
    fn graph_returns_relation_endpoints_and_edges() {
        let r = fixture();
        let g = r.with_conn(queries::graph).unwrap();

        // only the two endpoints of the one relation are nodes; the isolated
        // tweak mod (no relation) is omitted -- this is the relation graph
        assert_eq!(g.nodes.len(), 2);
        let apple = g
            .nodes
            .iter()
            .find(|n| n.modid.as_deref() == Some("appleskin"))
            .expect("appleskin node");
        let jei = g
            .nodes
            .iter()
            .find(|n| n.modid.as_deref() == Some("jei"))
            .expect("jei node");
        // appleskin carries a Modrinth id, jei is modid-only
        assert!(apple.modrinth);
        assert!(!jei.modrinth);

        // one edge jei -> appleskin, target resolved to the mod id
        assert_eq!(g.edges.len(), 1);
        let e = &g.edges[0];
        assert_eq!(e.from_mod_id, jei.mod_id);
        assert_eq!(e.to_mod_id, Some(apple.mod_id));
        assert_eq!(e.kind, "requires");
        assert_eq!(e.source, "jar-meta");
    }

    #[test]
    fn selector_resolves_through_forge_range_and_modrinth() {
        let r = fixture();
        r.with_conn(|c| {
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            assert_eq!(queries::mod_id_for_selector(c, "appleskin")?, Some(apple));
            // a Forge `modid@[range]` selector resolves like the bare modid (#1)
            assert_eq!(
                queries::mod_id_for_selector(c, "appleskin@[2.5,)")?,
                Some(apple)
            );
            // a `modrinth:<id>` selector resolves, with or without a range (#2)
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:EsAfb37o")?,
                Some(apple)
            );
            assert_eq!(
                queries::mod_id_for_selector(c, "modrinth:EsAfb37o@[2.5,)")?,
                Some(apple)
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn slice_graph_dedupes_a_dep_declared_two_ways() {
        let r = fixture();
        // jei already requires appleskin by bare modid; add the Forge range form of
        // the very same dependency, which used to resolve on its own and draw a
        // duplicate placeholder next to the resolved node (#1).
        r.with_conn_mut(|c| {
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            upsert::upsert_relation(
                c,
                jei,
                None,
                "appleskin@[2.5,)",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            let g = queries::graph_for_slice(c, None, Some("forge"))?;
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            let to_apple = g
                .edges
                .iter()
                .filter(|e| e.from_mod_id == jei && e.to_mod_id == Some(apple))
                .count();
            assert_eq!(to_apple, 1, "the same dep two ways collapses to one edge");
            assert!(
                !g.edges
                    .iter()
                    .any(|e| e.to_mod_id.is_none() && e.target.contains("appleskin")),
                "no dangling placeholder for a dep that resolves"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mod_edges_dedupe_a_dep_declared_two_ways() {
        let r = fixture();
        // jei already requires appleskin by bare modid; the Forge range form of the
        // same dep now resolves too and would list appleskin twice on the page (#1).
        r.with_conn_mut(|c| {
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            upsert::upsert_relation(
                c,
                jei,
                None,
                "appleskin@[2.5,)",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            let edges = queries::edges_for_mod(c, jei)?;
            let out_to_apple = edges
                .iter()
                .filter(|e| e.dir == "out" && e.other_name == "appleskin")
                .count();
            assert_eq!(out_to_apple, 1, "a dep declared two ways lists once");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_mods_carries_an_icon_source() {
        let r = fixture();
        r.with_conn(|c| {
            let mods = queries::list_mods(c, None, None, None, None, None)?;
            let apple = mods
                .iter()
                .find(|m| m.name == "appleskin")
                .expect("appleskin");
            // a Modrinth-catalogued mod offers its project id for the project icon
            assert_eq!(apple.modrinth_project_id.as_deref(), Some("EsAfb37o"));
            assert_eq!(apple.icon_sha1.as_deref(), Some("sha_apple"));
            // a modid-only mod has no project id, only its newest jar's sha1
            let jei = mods.iter().find(|m| m.name == "jei").expect("jei");
            assert_eq!(jei.modrinth_project_id, None);
            assert_eq!(jei.icon_sha1.as_deref(), Some("sha_jei"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mod_edges_include_a_range_suffixed_incoming_reference() {
        let r = fixture();
        // a mod that requires appleskin by the Forge `modid@[range]` form should
        // still be counted as an incoming edge on appleskin's page (#1 tail).
        r.with_conn_mut(|c| {
            let foo = upsert::upsert_mod_by_alias(c, &[("modid", "foo")], NOW)?;
            upsert::upsert_relation(
                c,
                foo,
                None,
                "appleskin@[2.5,)",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let edges = queries::edges_for_mod(c, apple)?;
            assert!(
                edges.iter().any(|e| e.dir == "in" && e.other_name == "foo"),
                "a range-suffixed incoming reference is counted"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn used_by_finds_a_modrinth_only_mod_in_a_build() {
        let r = fixture();
        // a mod known only by its Modrinth project id -- no modid alias, the case
        // that reported "used in no pack" though a build ships it (#18).
        r.with_conn_mut(|c| {
            let build: i64 = c.query_row(
                "SELECT id FROM pack_build WHERE pack_id = 'Industrial'",
                [],
                |r| r.get(0),
            )?;
            let m = upsert::upsert_mod_by_alias(c, &[("modrinth", "AANobbMI")], NOW)?;
            let v = upsert::upsert_mod_version(
                c,
                m,
                "1.0",
                &["forge"],
                "sha_mb",
                700,
                Some("mixinbooter.jar"),
                None,
                NOW,
            )?;
            upsert::link_build_mod(c, build, v, "mixinbooter.jar", true, true)?;
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            let m = queries::mod_id_for_alias(c, "modrinth", "AANobbMI")?.unwrap();
            let detail = queries::mod_detail(c, m)?.expect("mod detail");
            assert_eq!(
                detail.used_by.len(),
                1,
                "a Modrinth-only mod in a build is used by it"
            );
            assert_eq!(detail.used_by[0].pack_id, "Industrial");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn alias_collapse_one_identity() {
        let r = fixture();
        r.with_conn(|c| {
            let by_modid = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let by_project = queries::mod_id_for_alias(c, "modrinth", "EsAfb37o")?.unwrap();
            assert_eq!(
                by_modid, by_project,
                "modid + project_id collapse to one mod"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q1_packs_using_mod() {
        let r = fixture();
        r.with_conn(|c| {
            let uses = queries::packs_using_mod(c, "modid", "appleskin")?;
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].pack_id, "Industrial");
            assert_eq!(uses[0].filename, "appleskin.jar");
            // reachable via the Modrinth alias too
            assert_eq!(
                queries::packs_using_mod(c, "modrinth", "EsAfb37o")?.len(),
                1
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q2_orphans_only_unreferenced() {
        let r = fixture();
        r.with_conn(|c| {
            let orphans = queries::orphan_jars(c)?;
            let shas: Vec<_> = orphans.iter().map(|o| o.sha1.as_str()).collect();
            assert!(shas.contains(&"sha_orphan"));
            assert!(shas.contains(&"sha_cr")); // cleanroom artifact is in no build
            assert!(!shas.contains(&"sha_apple"));
            assert!(!shas.contains(&"sha_jei"));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q3_versions_of_mod() {
        let r = fixture();
        r.with_conn(|c| {
            let vs = queries::versions_of_mod(c, "modid", "appleskin")?;
            assert_eq!(vs.len(), 1);
            assert_eq!(vs[0].version, "2.5.1");
            assert_eq!(vs[0].targets, vec!["forge".to_string()]);
            // the multi-loader jar folds both targets into one version row
            let ms = queries::versions_of_mod(c, "modid", "multimod")?;
            assert_eq!(ms.len(), 1);
            let mut t = ms[0].targets.clone();
            t.sort();
            assert_eq!(t, vec!["fabric".to_string(), "forge".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn releases_group_a_mods_files() {
        let r = fixture();
        r.with_conn(|c| {
            // appleskin: one backfilled release (2.5.1) holding its one file
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let rels = queries::releases_of_mod_by_id(c, apple)?;
            assert_eq!(rels.len(), 1);
            assert_eq!(rels[0].version_number, "2.5.1");
            assert_eq!(rels[0].channel, "unknown");
            assert_eq!(rels[0].files.len(), 1);
            assert_eq!(rels[0].files[0].sha1, "sha_apple");
            assert_eq!(rels[0].files[0].targets, vec!["forge".to_string()]);
            // the multi-loader jar: one release, one file carrying both targets
            let multi = queries::mod_id_for_alias(c, "modid", "multimod")?.unwrap();
            let mrels = queries::releases_of_mod_by_id(c, multi)?;
            assert_eq!(mrels.len(), 1);
            assert_eq!(mrels[0].files.len(), 1);
            let mut t = mrels[0].files[0].targets.clone();
            t.sort();
            assert_eq!(t, vec!["fabric".to_string(), "forge".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    /// The panel draws its provenance chip from the listing, not from a
    /// per-file lookup, so an identity that only `GET /v1/files/{sha1}` can see
    /// is one the panel cannot use: every CurseForge jar reads as self-hosted,
    /// which is the `else` of a question that was never asked here.
    #[test]
    fn a_files_listing_carries_what_curseforge_said() {
        let r = fixture();
        let found = crate::authoring::curseforge::Match {
            project_id: 243_121,
            file_id: 4_520_594,
            display_name: "AppleSkin 2.5.1".into(),
            file_name: "appleskin-2.5.1.jar".into(),
            download_url: Some("https://edge.forgecdn.net/x.jar".into()),
            game_versions: vec!["1.12.2".into()],
        };
        r.with_conn_mut(|c| {
            upsert::set_curseforge_file(c, "sha_apple", 2_910_837_341, Some(&found), NOW)?;
            // asked, and published nowhere: a different answer to "never asked",
            // and the only one that earns the self-hosted chip
            upsert::set_curseforge_file(c, "sha_jei", 7, None, NOW)?;
            Ok(())
        })
        .unwrap();

        r.with_conn(|c| {
            let apple = queries::mod_id_for_alias(c, "modid", "appleskin")?.unwrap();
            let cf = queries::releases_of_mod_by_id(c, apple)?[0].files[0]
                .curseforge
                .clone()
                .expect("the listing carries the identity, not just the file page");
            assert_eq!(cf.project_id, Some(243_121));
            assert_eq!(cf.display_name.as_deref(), Some("AppleSkin 2.5.1"));
            assert!(cf.distributable, "a download url means it may be served");

            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            let miss = queries::releases_of_mod_by_id(c, jei)?[0].files[0]
                .curseforge
                .clone()
                .expect("a miss is an answer and has to survive the join");
            assert_eq!(miss.project_id, None);
            assert!(!miss.distributable);

            // Never asked stays absent, so the panel can say so rather than
            // calling it unpublished.
            let multi = queries::mod_id_for_alias(c, "modid", "multimod")?.unwrap();
            assert!(
                queries::releases_of_mod_by_id(c, multi)?[0].files[0]
                    .curseforge
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn q4_family_reachability_is_directional() {
        let r = fixture();
        r.with_conn(|c| {
            let eligible = |loader: &str| -> Vec<String> {
                queries::eligible_for_loader(c, loader)
                    .unwrap()
                    .into_iter()
                    .map(|e| e.sha1)
                    .collect()
            };
            // cleanroom inherits forge -> forge + cleanroom + any + the multi jar
            let cr = eligible("cleanroom");
            for s in ["sha_apple", "sha_tweak", "sha_cr", "sha_multi"] {
                assert!(cr.contains(&s.to_string()), "cleanroom should see {s}");
            }
            // forge build does NOT pick up the cleanroom-only artifact, but does
            // see the forge+fabric jar (eligibility is per-target)
            let fo = eligible("forge");
            assert!(fo.contains(&"sha_apple".to_string()));
            assert!(fo.contains(&"sha_tweak".to_string())); // any
            assert!(fo.contains(&"sha_multi".to_string())); // forge of forge+fabric
            assert!(!fo.contains(&"sha_cr".to_string()));
            // fabric sees the multi jar via its fabric target + the any tweaker,
            // and none of the forge-only artifacts
            let fa = eligible("fabric");
            assert!(fa.contains(&"sha_multi".to_string()));
            assert!(fa.contains(&"sha_tweak".to_string()));
            assert!(!fa.contains(&"sha_apple".to_string()));
            assert!(!fa.contains(&"sha_cr".to_string()));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn never_clobber_authored_version() {
        let r = fixture();
        r.with_conn_mut(|c| {
            // promote jei's artifact to an authored row with a hand-set version
            c.execute(
                "UPDATE mod_version SET source = 'authored', version = 'AUTH' WHERE sha1 = 'sha_jei'",
                [],
            )?;
            // a re-harvest of the same sha1 must not overwrite it
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            upsert::upsert_mod_version(
                c, jei, "9.9.9", &["forge"], "sha_jei", 2000, Some("jei.jar"), None, NOW,
            )?;
            let v: String =
                c.query_row("SELECT version FROM mod_version WHERE sha1 = 'sha_jei'", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(v, "AUTH", "authored row left untouched by re-harvest");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn browser_list_mods_filters() {
        let r = fixture();
        r.with_conn(|c| {
            // unfiltered: every harvested mod, named by its modid (no canonical
            // name set in the fixture)
            let all = queries::list_mods(c, None, None, None, None, None)?;
            let names: Vec<_> = all.iter().map(|m| m.name.as_str()).collect();
            assert!(names.contains(&"appleskin"));
            assert!(names.contains(&"multimod"));
            assert_eq!(all.len(), 6);
            // a loader filter keeps `any` + that loader's artifacts
            let fabric = queries::list_mods(c, None, Some("fabric"), None, None, None)?;
            let fnames: Vec<_> = fabric.iter().map(|m| m.name.as_str()).collect();
            assert!(fnames.contains(&"multimod")); // forge+fabric jar
            assert!(fnames.contains(&"tweak")); // any
            assert!(!fnames.contains(&"appleskin")); // forge-only
            // a name query matches the modid alias
            let apple = queries::list_mods(c, Some("apple"), None, None, None, None)?;
            assert_eq!(apple.len(), 1);
            assert_eq!(apple[0].name, "appleskin");
            // loader filter is case-insensitive (pack loader "Forge" -> "forge")
            let upper = queries::list_mods(c, None, Some("Forge"), None, None, None)?;
            assert!(upper.iter().any(|m| m.name == "appleskin"));
            // and walks the family DAG: a cleanroom pack sees forge-only mods
            // (cleanroom inherits forge) plus its own + any, not fabric-only ones
            let cr = queries::list_mods(c, None, Some("cleanroom"), None, None, None)?;
            let crn: Vec<_> = cr.iter().map(|m| m.name.as_str()).collect();
            assert!(crn.contains(&"appleskin"), "forge mod visible to cleanroom");
            assert!(crn.contains(&"crmod"), "cleanroom-only mod visible");
            assert!(crn.contains(&"tweak"), "any-loader mod visible");
            // the multi-loader jar reports both loaders as facets
            let multi = all.iter().find(|m| m.name == "multimod").unwrap();
            assert_eq!(
                multi.loaders,
                vec!["fabric".to_string(), "forge".to_string()]
            );
            Ok(())
        })
        .unwrap();
    }

    // Paging must be a way of reading the same listing, not a different one:
    // walked two at a time it yields every mod exactly once, in the order the
    // whole listing has, and each page carries the same facet chips.
    #[test]
    fn browser_list_mods_pages_the_listing_it_would_have_answered() {
        let r = fixture();
        r.with_conn(|c| {
            let whole = queries::list_mods(c, None, None, None, None, None)?;
            let mut walked = Vec::new();
            let mut after = None;
            loop {
                let page = queries::list_mods(c, None, None, None, after, Some(2))?;
                if page.is_empty() {
                    break;
                }
                assert!(page.len() <= 2, "a page is the size it was asked for");
                after = page.last().map(|m| m.mod_id);
                walked.extend(page);
            }
            assert_eq!(
                walked.iter().map(|m| m.mod_id).collect::<Vec<_>>(),
                whole.iter().map(|m| m.mod_id).collect::<Vec<_>>()
            );
            // the facets are per page but say the same thing
            let multi = walked.iter().find(|m| m.name == "multimod").unwrap();
            assert_eq!(
                multi.loaders,
                vec!["fabric".to_string(), "forge".to_string()]
            );

            // a filtered listing pages within the filter
            let one = queries::list_mods(c, None, Some("cleanroom"), None, None, Some(1))?;
            assert_eq!(one.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn browser_list_builds_and_build_mods() {
        let r = fixture();
        r.with_conn(|c| {
            let builds = queries::list_builds(c)?;
            assert_eq!(builds.len(), 1);
            assert_eq!(builds[0].pack_id, "Industrial");
            assert_eq!(builds[0].mod_count, 3);
            assert!(builds[0].is_latest);

            let mods = queries::build_mods(c, "Industrial", "2026.06.06")?;
            assert_eq!(mods.len(), 3);
            let names: Vec<_> = mods.iter().map(|m| m.name.as_str()).collect();
            assert!(names.contains(&"appleskin"));
            assert!(names.contains(&"jei"));
            assert!(names.contains(&"tweak"));
            // each row resolves to the artifact sha1 the operator would re-add
            assert!(mods.iter().all(|m| !m.sha1.is_empty()));
            // tweak ships as optional in the fixture build
            let tweak = mods.iter().find(|m| m.name == "tweak").unwrap();
            assert!(!tweak.required);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn browser_mc_facet_and_filter() {
        // a dedicated registry so we control mc_versions (the shared fixture
        // leaves them unset)
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let m = upsert::upsert_mod_by_alias(c, &[("modid", "biomesoplenty")], NOW)?;
            upsert::upsert_mod_version(
                c,
                m,
                "7.0",
                &["forge"],
                "sha_bop",
                1000,
                Some("bop.jar"),
                Some(r#"["1.12.2"]"#),
                NOW,
            )?;
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            // the mc set folds into both the summary facet and the version row
            let hit = queries::list_mods(c, None, None, Some("1.12.2"), None, None)?;
            assert_eq!(hit.len(), 1);
            assert_eq!(hit[0].mc_versions, vec!["1.12.2".to_string()]);
            assert!(queries::list_mods(c, None, None, Some("1.20.1"), None, None)?.is_empty());
            let vs = queries::versions_of_mod(c, "modid", "biomesoplenty")?;
            assert_eq!(vs[0].mc_versions, vec!["1.12.2".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn browser_name_query_treats_underscore_literally() {
        // an unescaped LIKE would let `_` wildcard-match any char; with ESCAPE the
        // query 'iron_chests' must match only the literal-underscore mod, not the
        // sibling where '_' would stand in for 'x'
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            for (modid, sha) in [("iron_chests", "sha_ic"), ("ironxchests", "sha_ix")] {
                let m = upsert::upsert_mod_by_alias(c, &[("modid", modid)], NOW)?;
                upsert::upsert_mod_version(c, m, "1", &["forge"], sha, 1, None, None, NOW)?;
            }
            Ok(())
        })
        .unwrap();
        r.with_conn(|c| {
            let hits = queries::list_mods(c, Some("iron_chests"), None, None, None, None)?;
            assert_eq!(
                hits.len(),
                1,
                "underscore matched literally, not as a wildcard"
            );
            assert_eq!(hits[0].name, "iron_chests");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn relation_dedupe_on_reinsert() {
        let r = fixture();
        r.with_conn_mut(|c| {
            let jei = queries::mod_id_for_alias(c, "modid", "jei")?.unwrap();
            // same sourced assertion again -> ignored
            upsert::upsert_relation(c, jei, None, "appleskin", None, RelKind::Requires, None, Source::JarMeta, NOW)?;
            let n: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE from_mod_id = ?1 AND target_modid = 'appleskin'",
                [jei],
                |r| r.get(0),
            )?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    // #48: two artifacts of one mod each declare their own dependency. Asking about
    // one must never hand back the other's -- that union was the bug the artifact
    // scope exists to kill. Mod-level facts (an authored assertion about the mod)
    // still apply to every artifact, and an artifact the registry never read gets
    // those alone rather than borrowing a sibling build's.
    #[test]
    fn relations_are_scoped_to_the_artifact_that_declares_them() {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let m = upsert::upsert_mod_by_alias(c, &[("modid", "jei")], NOW)?;
            let old = upsert::upsert_mod_version(
                c,
                m,
                "4.15",
                &["forge"],
                "sha_old",
                1,
                None,
                None,
                NOW,
            )?;
            let new = upsert::upsert_mod_version(
                c,
                m,
                "4.16",
                &["forge"],
                "sha_new",
                1,
                None,
                None,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                m,
                Some(old),
                "oldlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                m,
                Some(new),
                "newlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                m,
                None,
                "always",
                None,
                RelKind::Conflicts,
                Some(Severity::Hard),
                Source::Authored,
                NOW,
            )?;

            let targets = |mv: i64| -> anyhow::Result<Vec<String>> {
                Ok(queries::relations_for_artifact(c, mv, m)?
                    .into_iter()
                    .map(|e| e.target)
                    .collect())
            };

            let a = targets(old)?;
            assert!(a.contains(&"oldlib".to_string()));
            assert!(
                !a.contains(&"newlib".to_string()),
                "a sibling version's dependency must not leak onto this artifact"
            );
            assert!(
                a.contains(&"always".to_string()),
                "a mod-level fact applies to every artifact"
            );

            let b = targets(new)?;
            assert!(b.contains(&"newlib".to_string()));
            assert!(!b.contains(&"oldlib".to_string()));

            // never harvested: mod-level facts only, no borrowed dependencies
            assert_eq!(targets(-1)?, vec!["always".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    // #49: the point of a slice. One mod with a 1.12.2 build and a 1.19.2 build,
    // each depending on something different, must not bleed across worlds -- which
    // is exactly what the unsliced union did.
    #[test]
    fn graph_slice_keeps_minecraft_worlds_apart() {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let jei = upsert::upsert_mod_by_alias(c, &[("modid", "jei")], NOW)?;
            let old = upsert::upsert_mod_version(
                c,
                jei,
                "4.15",
                &["forge"],
                "sha_old",
                1,
                None,
                Some(r#"["1.12.2"]"#),
                NOW,
            )?;
            let new = upsert::upsert_mod_version(
                c,
                jei,
                "9.0",
                &["forge"],
                "sha_new",
                1,
                None,
                Some(r#"["1.19.2"]"#),
                NOW,
            )?;
            // each world's target exists as its own mod, in that world only
            for (modid, sha, mc) in [
                ("oldlib", "sha_oldlib", r#"["1.12.2"]"#),
                ("newlib", "sha_newlib", r#"["1.19.2"]"#),
            ] {
                let m = upsert::upsert_mod_by_alias(c, &[("modid", modid)], NOW)?;
                upsert::upsert_mod_version(c, m, "1.0", &["forge"], sha, 1, None, Some(mc), NOW)?;
            }
            upsert::upsert_relation(
                c,
                jei,
                Some(old),
                "oldlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                jei,
                Some(new),
                "newlib",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;

            let targets = |mc: &str| -> anyhow::Result<Vec<String>> {
                Ok(queries::graph_for_slice(c, Some(mc), Some("forge"))?
                    .edges
                    .into_iter()
                    .map(|e| e.target)
                    .collect())
            };
            assert_eq!(targets("1.12.2")?, vec!["oldlib".to_string()]);
            assert_eq!(targets("1.19.2")?, vec!["newlib".to_string()]);

            // unsliced, both worlds' edges land in one picture -- which is the
            // union the slice exists to replace
            let all = queries::graph_for_slice(c, None, None)?;
            assert_eq!(all.edges.len(), 1, "one artifact per mod is still picked");
            Ok(())
        })
        .unwrap();
    }

    // A fork sees what it can actually run: a cleanroom slice reaches forge
    // artifacts through loader_parent, the same way eligibility does (#37).
    #[test]
    fn graph_slice_loader_match_follows_forks() {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let a = upsert::upsert_mod_by_alias(c, &[("modid", "aaa")], NOW)?;
            let av = upsert::upsert_mod_version(
                c,
                a,
                "1.0",
                &["forge"],
                "sha_a",
                1,
                None,
                Some(r#"["1.12.2"]"#),
                NOW,
            )?;
            let b = upsert::upsert_mod_by_alias(c, &[("modid", "bbb")], NOW)?;
            upsert::upsert_mod_version(
                c,
                b,
                "1.0",
                &["forge"],
                "sha_b",
                1,
                None,
                Some(r#"["1.12.2"]"#),
                NOW,
            )?;
            upsert::upsert_relation(
                c,
                a,
                Some(av),
                "bbb",
                None,
                RelKind::Requires,
                None,
                Source::JarMeta,
                NOW,
            )?;
            let edges = |loader: &str| -> anyhow::Result<usize> {
                Ok(queries::graph_for_slice(c, Some("1.12.2"), Some(loader))?
                    .edges
                    .len())
            };
            assert_eq!(edges("forge")?, 1);
            assert_eq!(edges("cleanroom")?, 1, "cleanroom inherits forge artifacts");
            assert_eq!(edges("fabric")?, 0, "fabric is not downstream of forge");
            Ok(())
        })
        .unwrap();
    }

    // The same edge may now exist once per artifact -- two builds of a version
    // really do declare different things -- while a repeat for the same artifact
    // still dedupes.
    #[test]
    fn same_edge_coexists_per_artifact_but_dedupes_within_one() {
        let r = Registry::open_in_memory().unwrap();
        r.with_conn_mut(|c| {
            let m = upsert::upsert_mod_by_alias(c, &[("modid", "mod")], NOW)?;
            let a =
                upsert::upsert_mod_version(c, m, "1.0", &["forge"], "sha_a", 1, None, None, NOW)?;
            let b =
                upsert::upsert_mod_version(c, m, "1.0", &["fabric"], "sha_b", 1, None, None, NOW)?;
            let write = |mv: i64| {
                upsert::upsert_relation(
                    c,
                    m,
                    Some(mv),
                    "lib",
                    None,
                    RelKind::Requires,
                    None,
                    Source::JarMeta,
                    NOW,
                )
            };
            assert!(write(a)?, "first artifact records it");
            assert!(write(b)?, "the other build records its own");
            assert!(!write(a)?, "a repeat for the same artifact dedupes");
            let n: i64 = c.query_row(
                "SELECT count(*) FROM relation WHERE target_modid = 'lib'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 2);
            Ok(())
        })
        .unwrap();
    }
}
