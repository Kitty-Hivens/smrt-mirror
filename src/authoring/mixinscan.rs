//! What a jar's mixins patch, and how badly it needs them to be there (#145).
//!
//! A mixin config is a jar's declaration that it will rewrite someone else's
//! classes at load time. When it says `"required": true`, a target that is not
//! present is fatal: the loader throws during init, and the crash report names
//! whichever mod first reached the missing class rather than the one that asked
//! for it. Twice in one day a published pack met this -- Sable against a Sodium
//! that had moved its options class, then Iris against the same Sodium.
//!
//! Neither was visible in the metadata. Both declare an open lower bound on
//! Sodium (`[0.6,)`), which every version satisfies, so the version-window check
//! passes them; nothing anywhere says "I reach into this class".
//!
//! **Only `required` configs are collected.** A soft config is a mod's optional
//! integration with something that may not be installed, and a missing target
//! there is the design working. Reporting those would bury the real finding
//! under every dormant compatibility hook in the pack.
//!
//! This module reads; deciding whether an absent target matters belongs to the
//! layer that knows what else the pack ships.

use super::archive::read_zip_entry;
use super::classfile::{ClassInfo, parse_class};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};

/// One `required` mixin config, and every class its mixins must be able to
/// resolve for it to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredMixins {
    /// The config's own filename, as the jar names it (`sable.mixins.json`).
    /// Carried so a finding can point at the declaration rather than at a mod.
    pub config: String,
    /// Binary names, deduped, in the order first seen.
    ///
    /// Both what the mixins patch and what they merely mention, because the
    /// difference does not survive class loading. Sable's crash proved it: its
    /// `SodiumWorldRendererMixin` targets `SodiumWorldRenderer`, which was still
    /// there, and died on `SodiumGameOptions`, which its body referenced and
    /// which the new Sodium had moved. Checking targets alone would have caught
    /// Iris and waved Sable through.
    ///
    /// Deliberately unfiltered here. A mixin references plenty the pack need not
    /// carry -- its own helpers, the game's own classes -- and deciding which of
    /// these somebody in this pack is supposed to provide needs the pack, which
    /// this module does not have.
    pub needs: Vec<String>,
}

#[derive(Deserialize)]
struct MixinConfigJson {
    /// Absent reads as `false`: the Mixin specification's default is that a
    /// config may fail to apply, and only a config that says otherwise is
    /// making a promise the pack has to keep.
    #[serde(default)]
    required: bool,
    #[serde(default)]
    package: String,
    #[serde(default)]
    mixins: Vec<String>,
    #[serde(default)]
    client: Vec<String>,
    #[serde(default)]
    server: Vec<String>,
}

#[derive(Deserialize)]
struct ModsTomlMixins {
    #[serde(default)]
    mixins: Vec<MixinEntry>,
}

#[derive(Deserialize)]
struct MixinEntry {
    #[serde(default)]
    config: Option<String>,
}

/// `fabric.mod.json` lists configs as bare strings or as objects with a
/// `config` key (the object form carries an `environment` this does not read:
/// a config required on one side is still required).
#[derive(Deserialize)]
#[serde(untagged)]
enum FabricMixin {
    Named(String),
    Object { config: String },
}

#[derive(Deserialize)]
struct FabricModJson {
    #[serde(default)]
    mixins: Vec<FabricMixin>,
}

/// Every `required` mixin config in a jar, with what it patches.
///
/// Best-effort throughout: a jar that is not a zip, a config that will not
/// parse, a mixin class that is not in the jar -- each is skipped rather than
/// failing the scan. A partial answer here means a partial check later, which
/// is honest; an error would mean no check at all for a pack that has one
/// awkward jar in it.
pub fn scan_jar(jar_bytes: &[u8]) -> Vec<RequiredMixins> {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(jar_bytes)) else {
        return Vec::new();
    };
    let mods_toml = read_entry(&mut zip, "META-INF/neoforge.mods.toml")
        .or_else(|| read_entry(&mut zip, "META-INF/mods.toml"));
    let fabric_json = read_entry(&mut zip, "fabric.mod.json");

    let mut configs: HashMap<String, Vec<u8>> = HashMap::new();
    let mut classes: Vec<ClassInfo> = Vec::new();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let Ok(name) = entry.name().map(|n| n.into_owned()) else {
            continue;
        };
        let size = entry.size();
        let Ok(bytes) = read_zip_entry(&mut entry, size, &name) else {
            continue;
        };
        if name.ends_with(".class") {
            if let Some(info) = parse_class(&bytes) {
                classes.push(info);
            }
        } else if super::bytecode::is_mixin_config_name(&name) {
            configs.insert(name, bytes);
        }
    }
    from_parts(
        mods_toml.as_deref(),
        fabric_json.as_deref(),
        &configs,
        &classes,
    )
}

/// The same reading, from pieces a caller has already taken out of the jar.
///
/// The harvest opens each jar once and parses every class as it goes; making it
/// open the zip a second time to ask this question would undo the reason
/// `read_jar` exists.
pub fn from_parts(
    mods_toml: Option<&[u8]>,
    fabric_json: Option<&[u8]>,
    configs: &HashMap<String, Vec<u8>>,
    classes: &[ClassInfo],
) -> Vec<RequiredMixins> {
    let by_name: HashMap<&str, &ClassInfo> =
        classes.iter().map(|c| (c.this_class.as_str(), c)).collect();
    let mut out = Vec::new();
    for name in declared_config_names(mods_toml, fabric_json) {
        let Some(raw) = configs.get(&name) else {
            continue; // a config the metadata names and the jar does not carry
        };
        let Ok(cfg) = serde_json::from_slice::<MixinConfigJson>(raw) else {
            continue;
        };
        if !cfg.required {
            continue;
        }
        let mut needs: Vec<String> = Vec::new();
        let prefix = cfg.package.trim().replace('.', "/");
        for simple in cfg.mixins.iter().chain(&cfg.client).chain(&cfg.server) {
            let path = mixin_class_name(&prefix, simple);
            let Some(info) = by_name.get(path.as_str()) else {
                continue; // a class the config names and the jar does not carry
            };
            for t in info.mixin_targets.iter().chain(&info.referenced) {
                if *t != info.this_class && !needs.contains(t) {
                    needs.push(t.clone());
                }
            }
        }
        if !needs.is_empty() {
            out.push(RequiredMixins {
                config: name,
                needs,
            });
        }
    }
    out
}

/// The config filenames a jar declares, from whichever metadata it ships. A
/// multi-loader jar carries both, and the same config named twice is one
/// config.
fn declared_config_names(mods_toml: Option<&[u8]>, fabric_json: Option<&[u8]>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |n: String| {
        let n = n.trim().to_string();
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    };
    if let Some(bytes) = mods_toml
        && let Ok(text) = std::str::from_utf8(bytes)
        && let Ok(parsed) = toml::from_str::<ModsTomlMixins>(text)
    {
        for e in parsed.mixins {
            if let Some(c) = e.config {
                push(c);
            }
        }
    }
    if let Some(bytes) = fabric_json
        && let Ok(parsed) = serde_json::from_slice::<FabricModJson>(bytes)
    {
        for m in parsed.mixins {
            push(match m {
                FabricMixin::Named(c) => c,
                FabricMixin::Object { config } => config,
            });
        }
    }
    names
}

/// `package` + the config's entry for one mixin, as a binary class name. The
/// entry is relative to the package and may itself be dotted for a nested one.
fn mixin_class_name(prefix: &str, simple: &str) -> String {
    let simple = simple.trim().replace('.', "/");
    if prefix.is_empty() {
        simple
    } else {
        format!("{prefix}/{simple}")
    }
}

fn read_entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let size = entry.size();
    read_zip_entry(&mut entry, size, name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::classfile::fixtures::{ClassSpec, build_class_spec, jar};

    fn config(required: bool, package: &str, mixins: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "required": required,
            "package": package,
            "mixins": mixins,
        }))
        .unwrap()
    }

    fn mixin(this: &str, targets: &[&str]) -> Vec<u8> {
        build_class_spec(&ClassSpec {
            this,
            mixin_value: targets,
            ..ClassSpec::default()
        })
    }

    fn toml_with(config_name: &str) -> Vec<u8> {
        format!("[[mixins]]\nconfig = \"{config_name}\"\n").into_bytes()
    }

    #[test]
    fn a_required_config_yields_what_its_mixins_patch() {
        let j = jar(&[
            ("META-INF/neoforge.mods.toml", &toml_with("a.mixins.json")),
            ("a.mixins.json", &config(true, "mod.mixin", &["FooMixin"])),
            (
                "mod/mixin/FooMixin.class",
                &mixin("mod/mixin/FooMixin", &["other/Foo"]),
            ),
        ]);
        let got = scan_jar(&j);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].config, "a.mixins.json");
        assert!(got[0].needs.contains(&"other/Foo".to_string()));
    }

    #[test]
    fn a_soft_config_is_not_collected() {
        // an optional integration with a mod that may not be installed: a
        // missing target there is the design working, not a broken pack
        let j = jar(&[
            ("META-INF/neoforge.mods.toml", &toml_with("a.mixins.json")),
            ("a.mixins.json", &config(false, "mod.mixin", &["FooMixin"])),
            (
                "mod/mixin/FooMixin.class",
                &mixin("mod/mixin/FooMixin", &["other/Foo"]),
            ),
        ]);
        assert!(scan_jar(&j).is_empty());
    }

    #[test]
    fn client_and_server_lists_count_too() {
        let cfg = serde_json::to_vec(&serde_json::json!({
            "required": true,
            "package": "mod.mixin",
            "client": ["ClientMixin"],
            "server": ["ServerMixin"],
        }))
        .unwrap();
        let j = jar(&[
            ("META-INF/neoforge.mods.toml", &toml_with("a.mixins.json")),
            ("a.mixins.json", &cfg),
            (
                "mod/mixin/ClientMixin.class",
                &mixin("mod/mixin/ClientMixin", &["other/Client"]),
            ),
            (
                "mod/mixin/ServerMixin.class",
                &mixin("mod/mixin/ServerMixin", &["other/Server"]),
            ),
        ]);
        let got = scan_jar(&j);
        assert_eq!(got.len(), 1);
        assert!(got[0].needs.contains(&"other/Client".to_string()));
        assert!(got[0].needs.contains(&"other/Server".to_string()));
    }

    #[test]
    fn a_fabric_jar_declares_its_configs_its_own_way() {
        let fmj = serde_json::to_vec(&serde_json::json!({
            "mixins": ["plain.mixins.json", {"config": "client.mixins.json"}],
        }))
        .unwrap();
        let j = jar(&[
            ("fabric.mod.json", &fmj),
            (
                "plain.mixins.json",
                &config(true, "mod.mixin", &["PlainMixin"]),
            ),
            (
                "client.mixins.json",
                &config(true, "mod.mixin", &["OtherMixin"]),
            ),
            (
                "mod/mixin/PlainMixin.class",
                &mixin("mod/mixin/PlainMixin", &["a/A"]),
            ),
            (
                "mod/mixin/OtherMixin.class",
                &mixin("mod/mixin/OtherMixin", &["b/B"]),
            ),
        ]);
        let got = scan_jar(&j);
        assert_eq!(got.len(), 2);
        assert!(got[0].needs.contains(&"a/A".to_string()));
        assert!(got[1].needs.contains(&"b/B".to_string()));
    }

    #[test]
    fn a_nested_package_in_the_entry_resolves() {
        let j = jar(&[
            ("META-INF/neoforge.mods.toml", &toml_with("a.mixins.json")),
            (
                "a.mixins.json",
                &config(true, "mod.mixin", &["deep.DeepMixin"]),
            ),
            (
                "mod/mixin/deep/DeepMixin.class",
                &mixin("mod/mixin/deep/DeepMixin", &["x/X"]),
            ),
        ]);
        assert!(scan_jar(&j)[0].needs.contains(&"x/X".to_string()));
    }

    #[test]
    fn a_jar_with_nothing_to_say_says_nothing() {
        assert!(scan_jar(b"not a zip").is_empty());
        assert!(scan_jar(&jar(&[("README", b"hi")])).is_empty());
        // a config the jar names but does not carry is skipped, not fatal
        let j = jar(&[("META-INF/neoforge.mods.toml", &toml_with("gone.json"))]);
        assert!(scan_jar(&j).is_empty());
    }

    /// Point `SMRT_MIXIN_JAR` at a real mod and print what it declares. Not a
    /// fixture: the unit tests above assemble the shapes, this one meets the
    /// awkwardness of a jar built by somebody else -- refmaps, mixin plugins,
    /// nested packages, configs listed in two metadata files at once.
    #[test]
    #[ignore = "needs a real jar (SMRT_MIXIN_JAR)"]
    fn scan_a_real_jar() {
        let path = std::env::var("SMRT_MIXIN_JAR").expect("SMRT_MIXIN_JAR");
        let bytes = std::fs::read(&path).expect("readable jar");
        // the harvest reads this from pieces it already has; the two paths must
        // not be allowed to drift apart
        assert_eq!(
            crate::authoring::harvest::read_jar(&bytes).required_mixins,
            scan_jar(&bytes),
            "read_jar and scan_jar disagree"
        );
        for c in scan_jar(&bytes) {
            println!("{} -- {} class(es) needed", c.config, c.needs.len());
            for t in &c.needs {
                println!("    {t}");
            }
        }
    }
}
