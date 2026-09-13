//! What a build is allowed to publish.
//!
//! A real build wrote the manifest, moved the `latest` pointer and rewrote the
//! summary card without anything asking whether the result held together (#108).
//! The launcher reads that pointer the moment it moves, so the first thing that
//! checked a broken pack was a player's crash log -- while the mirror already
//! knew: `resolve_pack` reads the same registry graph and reports exactly these
//! problems, but only when a curator pressed the button in the editor.
//!
//! This turns that report into a verdict. The cut is deliberate and narrow:
//! block on the two findings that mean *this pack cannot start*, record the rest
//! onto the build.
//!
//! - An unmet hard dependency is a crash on launch -- when something actually
//!   declared it. A bytecode-inferred edge is class-granularity evidence and
//!   cannot tell a hard dependency from an optional integration that merely
//!   references a foreign type, so an inferred-only miss is recorded, not
//!   enforced. Refusing a publish on a guess is how a gate loses its authority.
//! - An artifact built for a loader the pack does not run, with nothing present
//!   to bridge it, does not load at all -- and whatever needed it then breaks.
//! - A loader build outside the window a jar declares on it (#164). Same
//!   failure as the one below, one level down: the pin is a line in the pack's
//!   own config, the mods around it move on their own, and a mod that raises
//!   its floor stops the game before the main menu naming itself.
//! - A hard dependency present at a version outside the window its requirer
//!   declared. This was recorded rather than enforced at first, on the reasoning
//!   that such windows are written optimistically and the pack usually runs
//!   anyway. It does not: a loader reads the range out of the jar's own manifest
//!   and refuses to start. A pack shipped Sodium 0.6.13 under a dependant
//!   demanding `[0.8.12,)`, and the game died before the main menu, reporting a
//!   different mod entirely.
//!
//! Everything else is real information and not a verdict. An active conflict may
//! be exactly what the curator intends (two mods that overlap, one of them
//! shipped off by default at the launcher's discretion); an unidentified jar
//! means the check was partial, not that the pack is broken. Blocking on all of them would make the gate something
//! operators route around, which is worse than not having one.
//!
//! The override exists for the same reason: a curator who knows better than the
//! graph must be able to publish, and the mirror's job is then to say plainly
//! that it happened -- in the job log, in the audit trail, and on the manifest
//! itself.

use super::resolve::ResolveReport;
use crate::domain::BuildChecks;

/// Did something actually say this edge exists, or did the mirror derive it?
/// The declared tiers are a jar's own metadata, Modrinth, and anything a human
/// authored; `inferred` and `harvested` are the mirror's own reading of the
/// bytecode, which is evidence and not a declaration.
fn declared(source: &str) -> bool {
    !matches!(source, "inferred" | "harvested")
}

/// Judge a resolved pack. Lines are human sentences: nothing downstream parses
/// them, and the audience is whoever is reading the log or the manifest.
pub fn check(report: &ResolveReport) -> BuildChecks {
    let mut blocking = Vec::new();
    let mut advisory = Vec::new();

    for m in &report.missing {
        let window = m
            .version_range
            .as_deref()
            .map(|r| format!(" {r}"))
            .unwrap_or_default();
        let reason = match m.reason.as_deref() {
            Some("external") => " (it lives outside both Modrinth and the mirror)",
            _ => "",
        };
        let line = format!(
            "unmet hard dependency: {}{window} -- required by {} ({}){reason}",
            m.target,
            m.needed_by.join(", "),
            m.source,
        );
        if declared(&m.source) {
            blocking.push(line);
        } else {
            advisory.push(format!("{line}; derived from bytecode, so it may be an optional integration rather than a dependency"));
        }
    }

    // Two builds of one mod in an instance is a crash at load, not a risk to
    // weigh, so this blocks for the same reason a missing hard dependency does.
    for d in &report.duplicate_mods {
        blocking.push(format!(
            "{} is pinned {} times, as {} -- one instance cannot hold two builds of a mod",
            d.name,
            d.filenames.len(),
            d.filenames.join(" and "),
        ));
    }

    for l in &report.loader_mismatch {
        blocking.push(format!(
            "{} is built for {} -- this pack runs {} and nothing present bridges it",
            l.filename,
            l.artifact_loaders.join("/"),
            l.pack_loader,
        ));
    }

    // #145. Blocking rather than advisory, and not by choice: a required mixin
    // whose target is gone is not a risk to weigh but a game that stops during
    // init. There is no version of this that is a deliberate decision.
    for g in &report.mixin_gaps {
        blocking.push(format!(
            "{} patches {}, which the {} in this pack no longer has ({})",
            g.filename, g.needed, g.owner, g.config,
        ));
    }

    for c in &report.conflicts {
        advisory.push(format!(
            "{} and {} are marked {} ({}), and both are enabled by default",
            c.a,
            c.b,
            if c.hard {
                "incompatible"
            } else {
                "discouraged together"
            },
            c.source,
        ));
    }
    for c in &report.optional_conflicts {
        advisory.push(format!(
            "{} and {} are marked {} ({}); one ships opted out, so it only bites if that one is enabled",
            c.a,
            c.b,
            if c.hard { "incompatible" } else { "discouraged together" },
            c.source,
        ));
    }
    for v in &report.version_issues {
        blocking.push(format!(
            "{} ships {} for {}, outside the {} that {} declares",
            v.filename,
            v.present_version,
            v.target,
            v.required_range,
            v.needed_by.join(", "),
        ));
    }
    for l in &report.loader_version_issues {
        blocking.push(format!(
            "{} needs {} {}, and this pack pins {}",
            l.filename, l.loader, l.required_range, l.pack_version,
        ));
    }
    for f in &report.forced_client_attempts {
        advisory.push(format!(
            "{} is client-side, and {} declares a hard dependency on it ({}) -- a client mod is never force-installed, so that dependency is not enforced",
            f.filename,
            f.needed_by.join(", "),
            f.source,
        ));
    }
    if !report.unresolved.is_empty() {
        advisory.push(format!(
            "not identified in the registry, so nothing above was checked for {}: {}",
            report.unresolved.len(),
            report.unresolved.join(", "),
        ));
    }

    BuildChecks {
        blocking,
        advisory,
        overridden: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::resolve::{
        ActiveConflict, ForcedClientEdge, LoaderMismatch, MissingDep, MixinGap, VersionIssue,
    };

    fn empty() -> ResolveReport {
        ResolveReport {
            duplicate_mods: Vec::new(),
            declared_mods: 0,
            resolved_mods: 0,
            missing: vec![],
            conflicts: vec![],
            optional_conflicts: vec![],
            overlaps: vec![],
            version_issues: vec![],
            loader_version_issues: vec![],
            loader_mismatch: vec![],
            loader_bridged: vec![],
            mixin_gaps: vec![],
            unresolved: vec![],
            version_windows_unchecked: 0,
            coremods: vec![],
            unclassified: vec![],
            side_disagreements: vec![],
            forced_client_attempts: vec![],
            server_side: vec![],
            suggestions: vec![],
        }
    }

    // A clean pack carries no block at all: an empty `checks` object on every
    // manifest would be noise, and would make "nothing was found" look like
    // "something was recorded".
    // #145. Not advisory: a required mixin whose target is gone is a game that
    // stops during init, and there is no reading of it that is a choice.
    #[test]
    fn a_missing_mixin_target_stops_the_publish() {
        let mut r = empty();
        r.mixin_gaps = vec![MixinGap {
            filename: "Sable.jar".into(),
            config: "sable.mixins.json".into(),
            needed: "net/caffeinemc/mods/sodium/client/gui/SodiumGameOptions".into(),
            owner: "sodium.jar".into(),
        }];
        let checks = check(&r);
        assert_eq!(checks.blocking.len(), 1, "blocks: {:?}", checks.blocking);
        let line = &checks.blocking[0];
        // names all three: who asks, what is gone, and whose copy lacks it --
        // the crash report names none of them
        assert!(line.contains("Sable.jar"), "{line}");
        assert!(line.contains("SodiumGameOptions"), "{line}");
        assert!(line.contains("sodium.jar"), "{line}");
        assert!(line.contains("sable.mixins.json"), "{line}");
    }

    #[test]
    fn a_clean_pack_says_nothing() {
        let checks = check(&empty());
        assert!(checks.is_empty());
        assert!(checks.blocking.is_empty() && checks.advisory.is_empty());
    }

    // The first of the two that mean "cannot start". The line names the
    // requirer, because "something needs AE2" is not actionable.
    #[test]
    fn an_unmet_hard_dependency_blocks_and_names_who_needs_it() {
        let checks = check(&ResolveReport {
            missing: vec![MissingDep {
                target: "appliedenergistics2".into(),
                needed_by: vec!["ae2stuff.jar".into()],
                version_range: Some(">=0.44".into()),
                source: "jar-meta".into(),
                reason: None,
            }],
            ..empty()
        });
        assert_eq!(checks.blocking.len(), 1);
        let line = &checks.blocking[0];
        assert!(line.contains("appliedenergistics2"), "{line}");
        assert!(line.contains("ae2stuff.jar"), "names the requirer: {line}");
        assert!(line.contains(">=0.44"), "and the window: {line}");
        assert!(checks.advisory.is_empty());
    }

    // The second. A bridged foreign artifact loads, so it is not a finding at
    // all -- not blocking, and not recorded either.
    #[test]
    fn an_unbridged_loader_mismatch_blocks_but_a_bridged_one_is_not_a_finding() {
        let mismatch = LoaderMismatch {
            filename: "fab.jar".into(),
            artifact_loaders: vec!["fabric".into()],
            pack_loader: "forge".into(),
            bridged_by: None,
        };
        let checks = check(&ResolveReport {
            loader_mismatch: vec![mismatch.clone()],
            ..empty()
        });
        assert_eq!(checks.blocking.len(), 1);
        assert!(checks.blocking[0].contains("fab.jar"));
        assert!(checks.blocking[0].contains("fabric"));

        let bridged = check(&ResolveReport {
            loader_bridged: vec![LoaderMismatch {
                bridged_by: Some("connector.jar".into()),
                ..mismatch
            }],
            ..empty()
        });
        assert!(bridged.is_empty(), "a connector carries it: {bridged:?}");
    }

    // The finding that reached a player. A pack shipped Sodium 0.6.13 under a
    // dependant demanding [0.8.12,); the loader reads that range out of the
    // jar's own manifest, refused to start, and reported an unrelated mod as
    // the crash. Recorded rather than enforced, this went out.
    #[test]
    fn a_dependency_outside_its_declared_window_stops_a_publish() {
        let checks = check(&ResolveReport {
            version_issues: vec![VersionIssue {
                target: "sodium".into(),
                filename: "sodium.jar".into(),
                present_version: "0.6.13+mc1.21.1".into(),
                required_range: "[0.8.12,)".into(),
                needed_by: vec!["reeses-sodium-options.jar".into()],
            }],
            ..empty()
        });
        assert_eq!(checks.blocking.len(), 1, "{checks:?}");
        let line = &checks.blocking[0];
        assert!(line.contains("0.6.13"), "names what is shipped: {line}");
        assert!(line.contains("[0.8.12,)"), "and what is wanted: {line}");
        assert!(
            line.contains("reeses-sodium-options.jar"),
            "and who wants it, since that is the mod to change: {line}"
        );
        assert!(checks.advisory.is_empty());
    }

    // #164. The pin and the mod moved apart: JEI 19.42 raised its floor to
    // neoforge 21.1.238 while the pack still pinned 21.1.234, and the loader
    // stopped before the main menu. The line names the pin, which the crash
    // report never does -- it names the mod that asked and leaves the operator
    // to work out that a config line is what has to change.
    #[test]
    fn a_loader_pin_below_a_declared_floor_stops_a_publish() {
        let checks = check(&ResolveReport {
            loader_version_issues: vec![crate::authoring::resolve::LoaderVersionIssue {
                filename: "jei.jar".into(),
                loader: "neoforge".into(),
                pack_version: "21.1.234".into(),
                required_range: "[21.1.238,)".into(),
            }],
            ..empty()
        });
        assert_eq!(checks.blocking.len(), 1, "{checks:?}");
        let line = &checks.blocking[0];
        assert!(line.contains("jei.jar"), "names the jar: {line}");
        assert!(line.contains("[21.1.238,)"), "and what it wants: {line}");
        assert!(
            line.contains("21.1.234"),
            "and what the pack pins, which is the thing to change: {line}"
        );
        assert!(checks.advisory.is_empty());
    }

    // A hard edge the mirror derived from bytecode is evidence, not a
    // declaration: it cannot tell a dependency from an optional integration
    // that references a foreign type. Recorded, so the curator sees it; not
    // enforced, because refusing a publish on a guess is how a gate loses the
    // authority to refuse anything.
    #[test]
    fn an_inferred_miss_is_recorded_where_a_declared_one_blocks() {
        let miss = |source: &str| MissingDep {
            target: "somelib".into(),
            needed_by: vec!["mod.jar".into()],
            version_range: None,
            source: source.into(),
            reason: None,
        };
        for declared in ["jar-meta", "modrinth", "authored", "curator"] {
            let checks = check(&ResolveReport {
                missing: vec![miss(declared)],
                ..empty()
            });
            assert_eq!(checks.blocking.len(), 1, "{declared} is a declaration");
        }
        let checks = check(&ResolveReport {
            missing: vec![miss("inferred")],
            ..empty()
        });
        assert!(
            checks.blocking.is_empty(),
            "a derived edge does not refuse a publish: {:?}",
            checks.blocking
        );
        assert_eq!(checks.advisory.len(), 1);
        assert!(checks.advisory[0].contains("somelib"));
    }

    // Everything else is recorded, never enforced -- a conflict the curator
    // shipped on purpose must not need an override to publish.
    #[test]
    fn conflicts_versions_and_unidentified_jars_are_recorded_not_blocked() {
        let checks = check(&ResolveReport {
            conflicts: vec![ActiveConflict {
                a: "VoxelMap.jar".into(),
                b: "Xaeros.jar".into(),
                hard: false,
                source: "authored".into(),
            }],
            optional_conflicts: vec![ActiveConflict {
                a: "OptiFine.jar".into(),
                b: "Angelica.jar".into(),
                hard: true,
                source: "authored".into(),
            }],

            forced_client_attempts: vec![ForcedClientEdge {
                filename: "OptiFine.jar".into(),
                needed_by: vec!["shaderpack.jar".into()],
                source: "authored".into(),
            }],
            unresolved: vec!["mystery.jar".into(), "other.jar".into()],
            ..empty()
        });
        assert!(
            checks.blocking.is_empty(),
            "none of these stop a publish: {:?}",
            checks.blocking
        );
        assert_eq!(checks.advisory.len(), 4);
        let all = checks.advisory.join("\n");
        for expected in [
            "VoxelMap.jar",
            "Angelica.jar",
            "OptiFine.jar",
            "mystery.jar",
        ] {
            assert!(all.contains(expected), "missing {expected} in:\n{all}");
        }
    }
}
