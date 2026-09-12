//! Jar-level derivation: reduce a jar's `.class` entries (via [`classfile`]) to
//! the facts the registry stores -- which package prefixes the jar OWNS, which
//! other mods' prefixes it references (hard vs soft), what kind of thing the
//! jar is (a mod, a coremod, a bare library), and its side + server-match
//! policy classification.
//!
//! Hard vs soft is decided at class granularity: a referenced prefix is a *soft*
//! (optional) dependency only when every class that references it is conditional
//! integration code (an `isModLoaded` guard / `@Optional` / plugin marker). One
//! unconditional reference makes it a *hard* dependency. The package->owner join
//! that turns these prefixes into edges lives in the registry (`harvest`), since
//! it needs the index built from every jar.
//!
//! Classification reads the bytecode as the arbiter ("metadata often lies"),
//! with declared metadata as auxiliary signals, and refuses to guess: an axis
//! the signals cannot decide stays `None`, which the resolve layer reports as
//! `unclassified` rather than silently defaulting.

use super::archive::read_zip_entry;
use super::classfile::{ClassInfo, Dist, parse_class};
use crate::domain::{MatchPolicy, SideClass};
use std::collections::BTreeSet;
use std::io::Cursor;

/// What a jar is, decided before any side/policy question: a mod (it has a mod
/// identity -- an `@Mod` class or a metadata-file identity), a coremod (no mod
/// identity, but launch-plugin markers: `FMLCorePlugin`/`TweakClass` manifest
/// attributes, an `IFMLLoadingPlugin` implementor, or mixin configs), or a bare
/// library (neither). Coremods and libraries are never force-installed: their
/// presence class is `coremod`, always toggleable, flagged in the resolve
/// report. A jar carrying both a coremod and a mod identity classifies as a
/// mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JarKind {
    Mod,
    Coremod,
    Library,
}

impl JarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JarKind::Mod => "mod",
            JarKind::Coremod => "coremod",
            JarKind::Library => "library",
        }
    }
}

/// How solid a side verdict is: `High` -- an explicit marker decided it (@Mod
/// side flags, fabric env/entrypoints, a dist blanket, content registration);
/// `Low` -- the blanket client-surface heuristic, which reads a client-heavy
/// library (bspkrsCore-class) as client. The client invariant refuses a build
/// only over a high-confidence verdict; a declared hard edge outweighs a low
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideConfidence {
    High,
    Low,
}

impl SideConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            SideConfidence::High => "high",
            SideConfidence::Low => "low",
        }
    }
}

/// Meta-level signals the zip walk collects beside the classes, feeding the
/// classification.
#[derive(Debug, Clone, Default)]
pub struct JarSignals {
    /// MANIFEST.MF carries an `FMLCorePlugin` attribute.
    pub manifest_coremod: bool,
    /// MANIFEST.MF carries a `TweakClass` attribute.
    pub manifest_tweaker: bool,
    /// Number of `*.mixins.json` / `mixins.*.json` entries in the jar.
    pub mixin_configs: usize,
    /// `fabric.mod.json` `environment`, mapped (`*` -> Both).
    pub fabric_env: Option<SideClass>,
    /// Fabric entrypoint shape: a client entrypoint with no main/server one is
    /// a client-side hint (intermediary class names defeat package analysis).
    pub fabric_client_entrypoint: bool,
    pub fabric_main_entrypoint: bool,
    /// The jar declares a mod identity in metadata (mcmod.info modid, mods.toml
    /// modId, fabric id).
    pub meta_identity: bool,
    /// `mods.toml` `displayTest` tolerates an absent/mismatched server.
    pub display_test_tolerant: bool,
    /// JVM runtime languages the jar makes available on the classpath: a
    /// `ContainedDeps` scala-library / kotlin-stdlib entry in the manifest, or
    /// the runtime classes bundled loose in the jar. The provider signal.
    pub provided_runtimes: BTreeSet<String>,
}

/// Diagnostic counters behind a classification -- logged via `tracing` and
/// dumped by the corpus runner, so a wrong verdict can be argued with instead
/// of re-derived by hand.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub classes: usize,
    /// Classes with a content-registration signal (Block/Item/Entity/worldgen
    /// supertypes, GameRegistry / RegistryEvent / DeferredRegister references),
    /// not counting dist=CLIENT-annotated classes.
    pub content_classes: usize,
    /// Classes referencing `net/minecraft/client/**` (or dist=CLIENT-annotated).
    pub client_classes: usize,
    pub dist_client_classes: usize,
    pub dist_server_classes: usize,
    pub mod_annotations: usize,
    /// `@Mod` classes declaring `acceptableRemoteVersions = "*"`.
    pub arv_star: usize,
    pub sided_proxy: bool,
    pub loading_plugin_classes: usize,
    /// Classes touching `net/minecraft/**` at all -- the denominator of the
    /// blanket client analysis.
    pub mc_touching: usize,
}

/// The derivation facts one jar yields.
#[derive(Debug, Clone, Default)]
pub struct JarBytecode {
    /// Package prefixes this jar defines (its identity in the package index).
    pub owned: BTreeSet<String>,
    /// Referenced prefixes with at least one unconditional referencing class.
    pub hard_refs: BTreeSet<String>,
    /// Referenced prefixes referenced only from conditional (integration) classes.
    pub optional_refs: BTreeSet<String>,
    /// JVM runtime languages this jar needs on the classpath (`scala` / `kotlin`)
    /// but does not itself provide. `STOP_PREFIXES` keeps these out of the
    /// package-edge analysis -- they are not a mod -- so they are tracked here
    /// separately: a modern-fork loader (Cleanroom) ships no Scala/Kotlin, so a
    /// jar referencing one crashes at classload unless a provider is present.
    pub needs_runtime: BTreeSet<String>,
    /// JVM runtime languages this jar puts on the classpath for others: a
    /// `ContainedDeps` scala-library / kotlin-stdlib, or the runtime classes
    /// bundled loose. The provider side (Scalar for Scala 2.11).
    pub provides_runtime: BTreeSet<String>,
    /// Derived side, or `None` when the signals do not decide it.
    pub side: Option<SideClass>,
    /// How solid the side verdict is; `None` when there is no side.
    pub side_confidence: Option<SideConfidence>,
    /// Derived server-match policy, or `None` when undecided. A `None` policy
    /// on a Mod-kind jar is what the resolve layer reports `unclassified`.
    pub match_policy: Option<MatchPolicy>,
    /// What the jar is; decides the coremod presence branch.
    pub kind: Option<JarKind>,
    /// The `modid` from a class-level `@Mod` annotation, the identity fallback for
    /// a Forge mod that ships no `mcmod.info` / `mods.toml` (e.g. Chisel, HatStand).
    /// The first `@Mod`-carrying class wins; `None` when no class declares one.
    pub mod_id: Option<String>,
    pub evidence: Evidence,
}

/// First-segment roots too broad for a 2-segment prefix to be distinctive
/// (`com/author/mod`, not just `com/author`). Package-owning identity for these
/// needs the third segment.
const COMMON_ROOTS: &[&str] = &[
    "com", "net", "org", "io", "me", "dev", "gnu", "cpw", "cn", "ru", "pl", "fr", "de", "eu", "co",
    "uk", "tv", "xyz", "info", "mod", "mods", "cc", "gg", "app", "team", "site", "moe", "su",
];

/// Platform + common-library namespaces that are never a mod's identity. A class
/// under any of these is neither owned nor a dependency edge. Kept specific (e.g.
/// `com/google`, not all of `com`) so real mods under those roots still index.
const STOP_PREFIXES: &[&str] = &[
    "java",
    "javax",
    "jdk",
    "sun",
    "com/sun",
    "kotlin",
    "scala",
    "groovy",
    "net/minecraft",
    "net/minecraftforge",
    "cpw/mods/fml",
    "cpw/mods/modlauncher",
    "net/neoforged",
    "net/fabricmc",
    "org/quiltmc",
    "com/mojang",
    "org/lwjgl",
    "org/lwjglx",
    "com/ibm/icu",
    "org/apache",
    "org/slf4j",
    "org/objectweb/asm",
    "org/ow2/asm",
    "org/spongepowered",
    "com/google",
    "io/netty",
    "it/unimi/dsi",
    "gnu/trove",
    "org/joml",
    "com/typesafe",
    "oshi",
    "org/jline",
    "joptsimple",
    "org/checkerframework",
    "org/intellij",
    "org/jetbrains",
    "javassist",
    "paulscode",
    "com/jcraft",
];

/// Content-registration supertype/interface prefixes: a class extending one of
/// these registers gameplay content that exists on both sides. MCP-era (1.12)
/// and Mojang-mapping (1.17+) names; `net/minecraft/client/**` is deliberately
/// absent (a GUI subclass is not content).
const CONTENT_SUPER_PREFIXES: &[&str] = &[
    "net/minecraft/block/",
    "net/minecraft/item/",
    "net/minecraft/entity/",
    "net/minecraft/tileentity/",
    "net/minecraft/potion/",
    "net/minecraft/enchantment/",
    "net/minecraft/world/gen/",
    "net/minecraft/world/level/block/",
    "net/minecraft/world/item/",
    "net/minecraft/world/entity/",
    "net/minecraft/world/level/levelgen/",
];

/// Registry-writing API types: a reference means the jar registers content.
/// Read-side registry types (`ForgeRegistries`) are deliberately absent --
/// item viewers iterate registries without owning any content.
const REGISTRY_REFS: &[&str] = &[
    "net/minecraftforge/fml/common/registry/GameRegistry",
    "cpw/mods/fml/common/registry/GameRegistry",
    "net/minecraftforge/event/RegistryEvent",
    "net/minecraftforge/event/RegistryEvent$Register",
    "net/minecraftforge/registries/DeferredRegister",
    "net/neoforged/neoforge/registries/DeferredRegister",
];

/// Reduce a jar to its derivation facts. Best-effort: an unreadable jar or
/// unparseable class contributes nothing rather than failing. The zip-walking
/// shell around [`aggregate`]; the harvest single-pass reader builds richer
/// [`JarSignals`] (metadata identity, manifest attributes) itself.
pub fn scan_jar(jar_bytes: &[u8]) -> JarBytecode {
    let classes = read_classes(jar_bytes);
    let signals = scan_signals(jar_bytes);
    aggregate(&classes, &signals)
}

/// Collect the non-class signals [`scan_jar`] can see on its own: the manifest
/// coremod attributes, mixin configs, and the fabric env. (Metadata identity
/// needs the mcmod/toml parsers, which live with the harvest reader.)
fn scan_signals(jar_bytes: &[u8]) -> JarSignals {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(jar_bytes)) else {
        return JarSignals::default();
    };
    let mut signals = JarSignals::default();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        if is_mixin_config_name(&name) {
            signals.mixin_configs += 1;
        }
        // A jar that ships the runtime classes loose (`scala/...`, `kotlin/...`)
        // provides that language to the classpath itself.
        if name.ends_with(".class")
            && let Some(lang) = runtime_lang(&name)
        {
            signals.provided_runtimes.insert(lang.to_string());
        }
        match name.as_str() {
            "META-INF/MANIFEST.MF" => {
                let size = entry.size();
                if let Ok(raw) = read_zip_entry(&mut entry, size, &name) {
                    let (coremod, tweaker) = manifest_markers(&raw);
                    signals.manifest_coremod = coremod;
                    signals.manifest_tweaker = tweaker;
                    // A coremod that carries the runtime as `ContainedDeps`
                    // (scala-library-*.jar, kotlin-stdlib-*.jar) provides it at
                    // boot -- the Scalar mechanism.
                    for lang in manifest_contained_runtimes(&raw) {
                        signals.provided_runtimes.insert(lang);
                    }
                }
            }
            "fabric.mod.json" => {
                let size = entry.size();
                if let Ok(raw) = read_zip_entry(&mut entry, size, &name) {
                    let meta = super::modmeta::parse_fabric_json(&raw);
                    apply_fabric_meta(&mut signals, &meta);
                    signals.meta_identity |= meta.modid.is_some();
                }
            }
            _ => {}
        }
    }
    signals
}

/// Fold a parsed `fabric.mod.json` into the signals (env + entrypoint shape).
pub(crate) fn apply_fabric_meta(signals: &mut JarSignals, meta: &super::modmeta::ModMeta) {
    signals.fabric_env = match meta.environment.as_deref() {
        Some("*") => Some(SideClass::Both),
        Some("client") => Some(SideClass::Client),
        Some("server") => Some(SideClass::Server),
        _ => None,
    };
    signals.fabric_client_entrypoint = meta.client_entrypoint;
    signals.fabric_main_entrypoint = meta.main_entrypoint;
}

/// `FMLCorePlugin` / `TweakClass` main-attribute markers in a MANIFEST.MF body.
pub(crate) fn manifest_markers(raw: &[u8]) -> (bool, bool) {
    let text = String::from_utf8_lossy(raw);
    let mut coremod = false;
    let mut tweaker = false;
    for line in text.lines() {
        if line.starts_with("FMLCorePlugin:") || line.starts_with("FMLCorePlugin :") {
            coremod = true;
        }
        if line.starts_with("TweakClass:") || line.starts_with("TweakClass :") {
            tweaker = true;
        }
    }
    (coremod, tweaker)
}

/// The JVM runtime language a class/package name belongs to (`scala` / `kotlin`)
/// -- the two languages a legacy Forge jar needs on the classpath that a
/// modern-fork loader does not bundle. `None` for everything else. These sit in
/// `STOP_PREFIXES` (never a mod identity), so this is the only place they read
/// as a signal rather than being dropped.
pub(crate) fn runtime_lang(binary: &str) -> Option<&'static str> {
    if starts_with_segment(binary, "scala") {
        Some("scala")
    } else if starts_with_segment(binary, "kotlin") {
        Some("kotlin")
    } else {
        None
    }
}

/// Runtime languages a jar bundles as Forge `ContainedDeps`: nested library
/// jars named in the `ContainedDeps` main attribute, extracted onto the
/// classpath by the jar's loading plugin at boot (how Scalar ships Scala 2.11).
/// A `scala-library` / `scala-reflect` entry means scala; a `kotlin-stdlib` /
/// `kotlin-runtime` / `kotlin-reflect` entry means kotlin.
pub(crate) fn manifest_contained_runtimes(raw: &[u8]) -> BTreeSet<String> {
    let text = String::from_utf8_lossy(raw);
    let mut out = BTreeSet::new();
    // ContainedDeps is a space-separated list; the manifest may wrap it across
    // continuation lines (` ` at column 0), so scan the whole body for the jar
    // names rather than a single line. Gate on the attribute being present so a
    // stray mention of a scala jar elsewhere is not read as a provider.
    let lower = text.to_ascii_lowercase();
    if !lower.contains("containeddeps") {
        return out;
    }
    if lower.contains("scala-library") || lower.contains("scala-reflect") {
        out.insert("scala".to_string());
    }
    if lower.contains("kotlin-stdlib")
        || lower.contains("kotlin-runtime")
        || lower.contains("kotlin-reflect")
    {
        out.insert("kotlin".to_string());
    }
    out
}

/// A mixin configuration resource: `<anything>.mixins.json` or
/// `mixins.<anything>.json`, at any depth (they conventionally sit at the
/// root). A bare `mixins.json` matches neither convention distinctly and is
/// ignored.
pub(crate) fn is_mixin_config_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    (base.ends_with(".mixins.json") && base.len() > ".mixins.json".len())
        || (base.starts_with("mixins.")
            && base.ends_with(".json")
            && base.len() > "mixins.".len() + ".json".len())
}

/// Parse every `.class` entry in the jar. Non-zip / non-class / malformed entries
/// are skipped.
fn read_classes(jar_bytes: &[u8]) -> Vec<ClassInfo> {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(jar_bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        if !name.ends_with(".class") {
            continue;
        }
        let size = entry.size();
        let Ok(bytes) = read_zip_entry(&mut entry, size, &name) else {
            continue;
        };
        if let Some(info) = parse_class(&bytes) {
            out.push(info);
        }
    }
    out
}

/// Package segments that mark integration/compat code loaded behind a mod's
/// OWN plugin manager (AE2's IntegrationRegistry, RailCraft's plugin system),
/// which the `isModLoaded`/`@Optional` markers cannot see.
const INTEGRATION_SEGMENTS: &[&str] = &[
    "integration",
    "integrations",
    "compat",
    "compatibility",
    "plugins",
    "modsupport",
];

/// True when the class lives under an integration-marked package.
fn is_integration_class(binary: &str) -> bool {
    match binary.rfind('/') {
        Some(end) => binary[..end]
            .split('/')
            .any(|seg| INTEGRATION_SEGMENTS.contains(&seg)),
        None => false,
    }
}

/// Fold parsed classes + meta signals into the jar's facts. Pure -- the
/// unit-tested core; `scan_jar` and the harvest single-pass reader are only
/// zip-reading shells around it.
pub(crate) fn aggregate(classes: &[ClassInfo], signals: &JarSignals) -> JarBytecode {
    let mut owned: BTreeSet<String> = BTreeSet::new();
    for c in classes {
        if !is_platform(&c.this_class)
            && let Some(p) = package_prefix(&c.this_class)
        {
            owned.insert(p);
        }
    }

    // A jar that owns a foreign mod's `<root>/api` but nothing else under that
    // root is BUNDLING the API, not owning it (RailCraft ships ic2/api and
    // thaumcraft/api wholesale). Registering it would make this jar the sole
    // owner whenever the true owner is a never-scanned Modrinth pin, and every
    // consumer of that API would grow a false hard edge here. A mod that owns
    // its own API always owns sibling packages under the same root (verified
    // across the corpus), so the sibling test separates the two exactly. The
    // residual risk -- a standalone pure-API jar losing ownership -- errs in
    // the safe direction: a dropped edge, never an invented one.
    let bundled_apis: Vec<String> = owned
        .iter()
        .filter(|p| {
            let Some(root) = p.strip_suffix("/api") else {
                return false;
            };
            !owned.iter().any(|s| {
                s.as_str() != p.as_str()
                    && (s.as_str() == root
                        || (s.starts_with(root) && s[root.len()..].starts_with('/')))
            })
        })
        .cloned()
        .collect();
    for p in &bundled_apis {
        owned.remove(p);
    }

    // Integration-package classes count as conditional only when they are a
    // minority of the jar: a handful of compat modules inside a content mod is
    // embedded integration code, while a jar living entirely under an
    // integration namespace (ProjectRed-Integration) is simply named that way
    // and its references are its real dependencies.
    let integration_classes = classes
        .iter()
        .filter(|c| is_integration_class(&c.this_class))
        .count();
    let integration_is_minority = integration_classes * 2 < classes.len();

    let mut ref_all: BTreeSet<String> = BTreeSet::new();
    let mut ref_hard: BTreeSet<String> = BTreeSet::new();
    for c in classes {
        // dedup this class's referenced prefixes before grading
        let mut prefixes: BTreeSet<String> = BTreeSet::new();
        for r in &c.referenced {
            if is_platform(r) {
                continue;
            }
            if let Some(p) = package_prefix(r) {
                prefixes.insert(p);
            }
        }
        let conditional =
            c.conditional || (integration_is_minority && is_integration_class(&c.this_class));
        for p in prefixes {
            if !conditional {
                ref_hard.insert(p.clone());
            }
            ref_all.insert(p);
        }
    }

    // subtract owned so a jar never depends on itself -- and a bundled foreign
    // API is still self-shipped code, not a dependency on its true owner
    let mut hard_refs: BTreeSet<String> = ref_hard.difference(&owned).cloned().collect();
    let mut optional_refs: BTreeSet<String> = ref_all
        .difference(&ref_hard)
        .filter(|p| !owned.contains(*p))
        .cloned()
        .collect();
    for p in &bundled_apis {
        hard_refs.remove(p);
        optional_refs.remove(p);
    }

    // Classpath runtime languages (scala/kotlin). Referenced but never an edge
    // (STOP_PREFIXES), so gathered straight from the raw class references. A jar
    // that provides the runtime itself does not also need it.
    let provides_runtime: BTreeSet<String> = signals.provided_runtimes.clone();
    let mut needs_runtime: BTreeSet<String> = BTreeSet::new();
    for c in classes {
        for r in &c.referenced {
            if let Some(lang) = runtime_lang(r)
                && !provides_runtime.contains(lang)
            {
                needs_runtime.insert(lang.to_string());
            }
        }
    }

    let (side, side_confidence, match_policy, kind, evidence) = classify(classes, signals);
    JarBytecode {
        owned,
        hard_refs,
        optional_refs,
        needs_runtime,
        provides_runtime,
        side,
        side_confidence,
        match_policy,
        kind: Some(kind),
        mod_id: classes.iter().find_map(|c| c.mod_id.clone()),
        evidence,
    }
}

/// The classification core: kind first (identity vs coremod markers), then
/// side + match policy for a mod. Signal priority inside the bytecode branch:
/// explicit side annotations > fabric env > content registration > blanket
/// client analysis; an axis nothing decides stays `None` (never a guess).
type Verdict = (
    Option<SideClass>,
    Option<SideConfidence>,
    Option<MatchPolicy>,
    JarKind,
    Evidence,
);

fn classify(classes: &[ClassInfo], signals: &JarSignals) -> Verdict {
    let mut ev = Evidence {
        classes: classes.len(),
        sided_proxy: classes.iter().any(|c| c.sided_proxy),
        ..Evidence::default()
    };
    let (mut ann_client, mut ann_server) = (false, false);
    for c in classes {
        if c.mod_sides.is_some() {
            ev.mod_annotations += 1;
        }
        if let Some((cl, sv)) = c.mod_sides {
            ann_client |= cl;
            ann_server |= sv;
        }
        if c.acceptable_remote_versions.as_deref() == Some("*") {
            ev.arv_star += 1;
        }
        if c.loading_plugin {
            ev.loading_plugin_classes += 1;
        }
        match c.dist {
            Some(Dist::Client) => ev.dist_client_classes += 1,
            Some(Dist::Server) => ev.dist_server_classes += 1,
            None => {}
        }
        let client_refs = c
            .referenced
            .iter()
            .any(|r| r.starts_with("net/minecraft/client/"));
        if client_refs || c.dist == Some(Dist::Client) {
            ev.client_classes += 1;
        }
        // a class pinned to the client dist registers nothing the server needs
        if c.dist != Some(Dist::Client) && is_content_class(c) {
            ev.content_classes += 1;
        }
    }
    ev.mc_touching = mc_touching(classes);

    let has_mod_annotation = ev.mod_annotations > 0;
    let identity = has_mod_annotation || signals.meta_identity;
    let coremod_markers = signals.manifest_coremod
        || signals.manifest_tweaker
        || signals.mixin_configs > 0
        || ev.loading_plugin_classes > 0;
    let kind = if identity {
        JarKind::Mod
    } else if coremod_markers {
        JarKind::Coremod
    } else {
        JarKind::Library
    };
    if kind != JarKind::Mod {
        // not a mod: presence is the coremod branch, side/policy undecided
        return (None, None, None, kind, ev);
    }

    // 1. Explicit @Mod side flags (1.7-1.12), folded across bundled mods. A
    // bundle shipping a client-flagged part AND a server-flagged part is a
    // both-sides jar; its policy still comes from the content/marker logic.
    let mut side_pin: Option<SideClass> = None;
    if ann_client && !ann_server {
        return (
            Some(SideClass::Client),
            Some(SideConfidence::High),
            Some(MatchPolicy::Tolerant),
            kind,
            ev,
        );
    }
    if ann_server && !ann_client {
        return (
            Some(SideClass::Server),
            Some(SideConfidence::High),
            Some(MatchPolicy::Tolerant),
            kind,
            ev,
        );
    }
    if ann_client && ann_server {
        side_pin = Some(SideClass::Both);
    }

    // 2. Fabric environment: a declared single side decides both axes.
    match signals.fabric_env {
        Some(SideClass::Client) => {
            return (
                Some(SideClass::Client),
                Some(SideConfidence::High),
                Some(MatchPolicy::Tolerant),
                kind,
                ev,
            );
        }
        Some(SideClass::Server) => {
            return (
                Some(SideClass::Server),
                Some(SideConfidence::High),
                Some(MatchPolicy::Tolerant),
                kind,
                ev,
            );
        }
        _ => {}
    }

    // Tolerance markers: every bundled @Mod declares acceptableRemoteVersions
    // "*" (1.12), or the modern displayTest waives the server match. Worst
    // bundled mod wins, so one strict @Mod keeps the jar strict.
    let tolerant_marker = (ev.mod_annotations > 0 && ev.arv_star == ev.mod_annotations)
        || signals.display_test_tolerant;

    // 3. Content registration: gameplay content exists on both sides; without
    // a tolerance marker the server must carry it too.
    if ev.content_classes > 0 {
        let policy = if tolerant_marker {
            MatchPolicy::Tolerant
        } else {
            MatchPolicy::MustMatch
        };
        return (
            Some(SideClass::Both),
            Some(SideConfidence::High),
            Some(policy),
            kind,
            ev,
        );
    }

    // A declared tolerance marker without content: both-side, tolerant (the
    // JEI shape -- runs everywhere, server may lack it).
    if tolerant_marker {
        return (
            Some(SideClass::Both),
            Some(SideConfidence::High),
            Some(MatchPolicy::Tolerant),
            kind,
            ev,
        );
    }

    // 4. Blanket client analysis: no content, and the mod's Minecraft surface
    // is the client. A Fabric client entrypoint with no main one is the same
    // shape (intermediary names defeat the package check there).
    // A fabric client entrypoint with no main one is an author declaration;
    // the package-surface ratio alone is a heuristic that misreads a
    // client-heavy library (bspkrsCore-class) as client, so it grades Low.
    if signals.fabric_client_entrypoint && !signals.fabric_main_entrypoint {
        return (
            Some(SideClass::Client),
            Some(SideConfidence::High),
            Some(MatchPolicy::Tolerant),
            kind,
            ev,
        );
    }
    let client_only_shape = ev.client_classes > 0 && ev.dist_server_classes == 0;
    if client_only_shape && ev.client_classes * 2 >= ev.mc_touching {
        return (
            Some(SideClass::Client),
            Some(SideConfidence::Low),
            Some(MatchPolicy::Tolerant),
            kind,
            ev,
        );
    }

    // 5. A pinned Both (paired @Mod flags, fabric env "*") holds the side even
    // when the policy stays open -- the policy alone reports unclassified.
    if signals.fabric_env == Some(SideClass::Both) {
        side_pin = side_pin.or(Some(SideClass::Both));
    }
    let conf = side_pin.map(|_| SideConfidence::High);
    (side_pin, conf, None, kind, ev)
}

/// Classes that touch Minecraft at all (reference `net/minecraft/**`), the
/// denominator for the blanket client analysis: a client-only mod's
/// MC-touching classes are mostly client-touching ones.
fn mc_touching(classes: &[ClassInfo]) -> usize {
    classes
        .iter()
        .filter(|c| {
            c.referenced.iter().any(|r| r.starts_with("net/minecraft/"))
                || c.super_name
                    .as_deref()
                    .is_some_and(|s| s.starts_with("net/minecraft/"))
        })
        .count()
}

/// A content-registration class: extends/implements a content base, or
/// references a registry-writing API.
fn is_content_class(c: &ClassInfo) -> bool {
    let is_content_type = |name: &str| CONTENT_SUPER_PREFIXES.iter().any(|p| name.starts_with(p));
    if c.super_name.as_deref().is_some_and(is_content_type) {
        return true;
    }
    if c.interfaces
        .iter()
        .any(|i| i == "net/minecraftforge/fml/common/IWorldGenerator" || is_content_type(i))
    {
        return true;
    }
    c.referenced
        .iter()
        .any(|r| REGISTRY_REFS.contains(&r.as_str()))
}

/// The owning prefix for a binary class name: its package, trimmed to the first
/// two segments (three under a broad root). `None` for a default-package class.
pub(crate) fn package_prefix(binary: &str) -> Option<String> {
    let slash = binary.rfind('/')?;
    let pkg = &binary[..slash];
    if pkg.is_empty() {
        return None;
    }
    let segs: Vec<&str> = pkg.split('/').collect();
    let depth = if COMMON_ROOTS.contains(&segs[0]) {
        3
    } else {
        2
    };
    Some(segs[..depth.min(segs.len())].join("/"))
}

/// True when a binary name sits under a platform/library namespace (matched at a
/// `/` boundary so `net/minecraftforge` matches but `net/minecraftforgeX` does not).
pub(crate) fn is_platform(binary: &str) -> bool {
    STOP_PREFIXES.iter().any(|p| starts_with_segment(binary, p))
}

fn starts_with_segment(name: &str, prefix: &str) -> bool {
    name == prefix || (name.starts_with(prefix) && name.as_bytes().get(prefix.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::super::classfile::fixtures::{ClassSpec, build_class, build_class_spec, jar};
    use super::*;

    fn ci(this: &str, refs: &[&str], conditional: bool) -> ClassInfo {
        parse_class(&build_class(this, refs, conditional, None)).unwrap()
    }

    fn ci_spec(spec: &ClassSpec) -> ClassInfo {
        parse_class(&build_class_spec(spec)).unwrap()
    }

    fn mod_signals() -> JarSignals {
        JarSignals {
            meta_identity: true,
            ..JarSignals::default()
        }
    }

    #[test]
    fn package_prefix_depth_by_root() {
        assert_eq!(
            package_prefix("appeng/core/AppEng").as_deref(),
            Some("appeng/core")
        );
        assert_eq!(package_prefix("appeng/Api").as_deref(), Some("appeng"));
        // broad roots take a third segment
        assert_eq!(
            package_prefix("com/author/coolmod/Main").as_deref(),
            Some("com/author/coolmod")
        );
        assert_eq!(package_prefix("Toplevel").as_deref(), None);
    }

    #[test]
    fn platform_matches_at_segment_boundary() {
        assert!(is_platform("net/minecraft/block/Block"));
        assert!(is_platform("net/minecraftforge/fml/common/Loader"));
        assert!(is_platform("org/apache/logging/log4j/Logger"));
        // a real mod under a broad root is not platform
        assert!(!is_platform("appeng/core/AppEng"));
        assert!(!is_platform("com/author/mod/Main"));
        // no false prefix match past a segment boundary
        assert!(!is_platform("javaxtra/Foo"));
    }

    #[test]
    fn scala_reference_is_a_runtime_need_not_an_edge() {
        // A bdew-style jar (BdLib/AE2Stuff) references scala/ at classload. The
        // STOP_PREFIXES filter keeps it out of hard_refs, but it is a real
        // classpath need a modern-fork loader will not satisfy on its own.
        let classes = vec![ci(
            "bdlib/network/Packet",
            &["scala/collection/Seq", "cofh/api/energy/IEnergyHandler"],
            false,
        )];
        let out = aggregate(&classes, &mod_signals());
        assert!(out.needs_runtime.contains("scala"));
        assert!(out.provides_runtime.is_empty());
        // scala never reads as a dependency edge
        assert!(!out.hard_refs.iter().any(|p| p.starts_with("scala")));
        // an ordinary mod reference still resolves normally
        assert!(out.hard_refs.contains("cofh/api"));
    }

    #[test]
    fn kotlin_reference_is_a_runtime_need() {
        let classes = vec![ci("mymod/Main", &["kotlin/jvm/internal/Intrinsics"], false)];
        let out = aggregate(&classes, &mod_signals());
        assert!(out.needs_runtime.contains("kotlin"));
    }

    #[test]
    fn a_runtime_provider_does_not_also_need_it() {
        // Scalar references scala from its own plugin classes but ships the
        // runtime -- it is a provider, not a consumer, so it needs nothing.
        let mut signals = JarSignals::default();
        signals.provided_runtimes.insert("scala".to_string());
        let classes = vec![ci(
            "com/cleanroommc/scalar/Plugin",
            &["scala/Predef"],
            false,
        )];
        let out = aggregate(&classes, &signals);
        assert!(out.provides_runtime.contains("scala"));
        assert!(
            out.needs_runtime.is_empty(),
            "a provider is not also a consumer"
        );
    }

    #[test]
    fn contained_deps_manifest_reads_as_a_provider() {
        let scalar = b"Manifest-Version: 1.0\r\nFMLCorePlugin: com.cleanroommc.scalar.ScalarLoadingPlugin\r\nContainedDeps: scala-library-2.11.1.jar scala-reflect-2.11.1.jar\r\n";
        assert_eq!(
            manifest_contained_runtimes(scalar),
            BTreeSet::from(["scala".to_string()])
        );
        // no ContainedDeps -> not a provider, even if a scala jar is named elsewhere
        let plain = b"Manifest-Version: 1.0\r\nName: scala-library-note\r\n";
        assert!(manifest_contained_runtimes(plain).is_empty());
    }

    #[test]
    fn runtime_lang_matches_at_segment_boundary() {
        assert_eq!(runtime_lang("scala/collection/Seq"), Some("scala"));
        assert_eq!(runtime_lang("kotlin/Unit"), Some("kotlin"));
        assert_eq!(runtime_lang("scalaesque/Foo"), None);
        assert_eq!(runtime_lang("net/minecraft/block/Block"), None);
    }

    #[test]
    fn hard_reference_from_unconditional_class() {
        // ae2stuff-style: a core class references AE2 with no guard -> hard dep
        let classes = vec![ci("ae2stuff/core/Main", &["appeng/api/AEApi"], false)];
        let out = aggregate(&classes, &mod_signals());
        assert!(out.hard_refs.contains("appeng/api"));
        assert!(out.optional_refs.is_empty());
        assert!(out.owned.contains("ae2stuff/core"));
    }

    #[test]
    fn soft_reference_only_from_conditional_classes() {
        // every class touching JEI is integration code -> optional dep
        let classes = vec![
            ci("mymod/Main", &["mymod/Thing"], false),
            ci("mymod/compat/JeiPlugin", &["mezz/jei/api/IModPlugin"], true),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert!(out.optional_refs.contains("mezz/jei"));
        assert!(out.hard_refs.is_empty(), "no unconditional JEI reference");
    }

    #[test]
    fn one_hard_reference_overrides_a_soft_one() {
        // referenced both from a guarded class AND an unguarded one -> hard wins
        let classes = vec![
            ci("mymod/compat/Guarded", &["thermal/api/Energy"], true),
            ci("mymod/core/Core", &["thermal/api/Energy"], false),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert!(out.hard_refs.contains("thermal/api"));
        assert!(!out.optional_refs.contains("thermal/api"));
    }

    // Fix A: a jar shipping a foreign mod's bundled API (RailCraft carries
    // ic2/api + thaumcraft/api) neither owns that prefix nor depends on it; a
    // mod's OWN api package (sibling packages under the same root) stays owned.
    #[test]
    fn bundled_foreign_api_is_neither_owned_nor_an_edge() {
        let classes = vec![
            ci("mods/railcraft/common/Core", &[], false),
            ci("mods/railcraft/api/Track", &[], false),
            ci("ic2/api/EnergyNet", &[], false), // bundled: no ic2 siblings
            ci("thaumcraft/api/Aspect", &[], false),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert!(
            out.owned.contains("mods/railcraft/api"),
            "own api with siblings stays owned"
        );
        assert!(
            !out.owned.contains("ic2/api") && !out.owned.contains("thaumcraft/api"),
            "bundled foreign APIs are not owned: {:?}",
            out.owned
        );
        assert!(
            out.hard_refs.is_empty(),
            "self-shipped code is not a dependency either: {:?}",
            out.hard_refs
        );
    }

    // Fix B: integration-package classes (a minority of the jar) are compat
    // modules behind the mod's own plugin manager -- their foreign references
    // grade soft; a jar living entirely under an integration namespace
    // (ProjectRed-Integration) keeps its references hard.
    #[test]
    fn minority_integration_packages_grade_soft() {
        let classes = vec![
            ci(
                "mods/railcraft/common/Core",
                &["mods/railcraft/common/Track"],
                false,
            ),
            ci("mods/railcraft/common/Machine", &[], false),
            ci(
                "mods/railcraft/common/plugins/forestry/BackpackHandler",
                &["forestry/api/Backpack"],
                false,
            ),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert!(
            out.optional_refs.contains("forestry/api"),
            "a minority plugins package is integration code: {:?}",
            out.optional_refs
        );
        assert!(!out.hard_refs.contains("forestry/api"));

        // whole-jar integration namespace: the references are real deps
        let whole = vec![
            ci(
                "mrtjp/projectred/integration/GateLogic",
                &["codechicken/lib/Vec3"],
                false,
            ),
            ci("mrtjp/projectred/integration/Gates", &[], false),
        ];
        let out = aggregate(&whole, &mod_signals());
        assert!(
            out.hard_refs.contains("codechicken/lib"),
            "an integration-NAMED jar keeps its hard deps: {:?}",
            out.hard_refs
        );
    }

    #[test]
    fn own_and_platform_references_are_not_edges() {
        let classes = vec![ci(
            "mymod/core/Core",
            &[
                "mymod/core/Helper",
                "net/minecraft/block/Block",
                "java/util/List",
            ],
            false,
        )];
        let out = aggregate(&classes, &mod_signals());
        assert!(
            out.hard_refs.is_empty(),
            "self + platform refs produce no edge"
        );
        assert!(out.optional_refs.is_empty());
    }

    // ── D.1 matrix rows ─────────────────────────────────────────────────────

    #[test]
    fn mod_annotation_client_side_only_wins_over_fabric() {
        let c = ci_spec(&ClassSpec {
            this: "mymod/ClientMod",
            mod_sides: Some((true, false)),
            ..ClassSpec::default()
        });
        let signals = JarSignals {
            fabric_env: Some(SideClass::Both),
            meta_identity: true,
            ..JarSignals::default()
        };
        let out = aggregate(&[c], &signals);
        assert_eq!(out.side, Some(SideClass::Client));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
        assert_eq!(out.kind, Some(JarKind::Mod));
    }

    #[test]
    fn mod_annotation_server_side_only() {
        let c = ci_spec(&ClassSpec {
            this: "mymod/ServerMod",
            mod_sides: Some((false, true)),
            ..ClassSpec::default()
        });
        let out = aggregate(&[c], &mod_signals());
        assert_eq!(out.side, Some(SideClass::Server));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn fabric_env_decides_side_when_no_mod_annotation() {
        for (env, want) in [
            (SideClass::Client, SideClass::Client),
            (SideClass::Server, SideClass::Server),
        ] {
            let signals = JarSignals {
                fabric_env: Some(env),
                meta_identity: true,
                ..JarSignals::default()
            };
            let out = aggregate(&[ci("mymod/Main", &[], false)], &signals);
            assert_eq!(out.side, Some(want));
            assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
        }
    }

    #[test]
    fn content_registration_makes_a_must_match_both_mod() {
        // a block subclass and a DeferredRegister user both register content
        for spec in [
            ClassSpec {
                this: "mymod/BlockMachine",
                super_name: Some("net/minecraft/block/Block"),
                ..ClassSpec::default()
            },
            ClassSpec {
                this: "mymod/Registrar",
                refs: &["net/minecraftforge/registries/DeferredRegister"],
                ..ClassSpec::default()
            },
        ] {
            let out = aggregate(&[ci_spec(&spec)], &mod_signals());
            assert_eq!(out.side, Some(SideClass::Both), "{}", spec.this);
            assert_eq!(out.match_policy, Some(MatchPolicy::MustMatch));
        }
    }

    #[test]
    fn acceptable_remote_versions_star_makes_content_tolerant() {
        // JEI-shape: registers nothing server-critical, declares ARV "*"
        let main = ci_spec(&ClassSpec {
            this: "mezz/jei/JustEnoughItems",
            modid: Some("jei"),
            arv: Some("*"),
            ..ClassSpec::default()
        });
        let out = aggregate(&[main], &mod_signals());
        assert_eq!(out.side, Some(SideClass::Both));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));

        // even with content classes, the declared tolerance wins the policy
        let content = ci_spec(&ClassSpec {
            this: "mezz/jei/ItemThing",
            super_name: Some("net/minecraft/item/Item"),
            ..ClassSpec::default()
        });
        let main2 = ci_spec(&ClassSpec {
            this: "mezz/jei/JustEnoughItems",
            modid: Some("jei"),
            arv: Some("*"),
            ..ClassSpec::default()
        });
        let out2 = aggregate(&[main2, content], &mod_signals());
        assert_eq!(out2.side, Some(SideClass::Both));
        assert_eq!(out2.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn display_test_tolerant_is_the_modern_arv() {
        let content = ci_spec(&ClassSpec {
            this: "mymod/BlockThing",
            super_name: Some("net/minecraft/world/level/block/Block"),
            ..ClassSpec::default()
        });
        let signals = JarSignals {
            meta_identity: true,
            display_test_tolerant: true,
            ..JarSignals::default()
        };
        let out = aggregate(&[content], &signals);
        assert_eq!(out.side, Some(SideClass::Both));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn blanket_client_surface_classifies_client() {
        // no content, every MC-touching class touches the client packages
        let classes = vec![
            ci_spec(&ClassSpec {
                this: "mymod/gui/Overlay",
                refs: &["net/minecraft/client/gui/GuiScreen"],
                ..ClassSpec::default()
            }),
            ci_spec(&ClassSpec {
                this: "mymod/gui/Config",
                refs: &["net/minecraft/client/Minecraft"],
                ..ClassSpec::default()
            }),
            ci("mymod/util/Maths", &[], false),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert_eq!(out.side, Some(SideClass::Client));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn dist_client_annotations_count_as_client_surface() {
        let classes = vec![ci_spec(&ClassSpec {
            this: "mymod/HudRender",
            dist: Some(("Lnet/minecraftforge/api/distmarker/OnlyIn;", "CLIENT")),
            ..ClassSpec::default()
        })];
        let out = aggregate(&classes, &mod_signals());
        assert_eq!(out.side, Some(SideClass::Client));
    }

    #[test]
    fn fabric_client_entrypoint_without_main_is_client() {
        // intermediary class names defeat the package analysis; the entrypoint
        // shape is the fabric-side client signal
        let signals = JarSignals {
            meta_identity: true,
            fabric_client_entrypoint: true,
            fabric_main_entrypoint: false,
            ..JarSignals::default()
        };
        let out = aggregate(&[ci("mymod/Client", &[], false)], &signals);
        assert_eq!(out.side, Some(SideClass::Client));
        assert_eq!(out.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn content_mod_with_client_proxy_is_not_client() {
        // a real content mod also references client classes from its proxy;
        // the content signal must win over the client surface
        let classes = vec![
            ci_spec(&ClassSpec {
                this: "mymod/BlockMachine",
                super_name: Some("net/minecraft/block/Block"),
                ..ClassSpec::default()
            }),
            ci_spec(&ClassSpec {
                this: "mymod/ClientProxy",
                refs: &["net/minecraft/client/renderer/RenderItem"],
                ..ClassSpec::default()
            }),
        ];
        let out = aggregate(&classes, &mod_signals());
        assert_eq!(out.side, Some(SideClass::Both));
        assert_eq!(out.match_policy, Some(MatchPolicy::MustMatch));
    }

    #[test]
    fn dist_client_content_class_is_not_a_content_signal() {
        // a client-pinned class extending a render type must not read as content
        let classes = vec![ci_spec(&ClassSpec {
            this: "mymod/FancyItemRender",
            super_name: Some("net/minecraft/item/Item"),
            dist: Some(("Lnet/minecraftforge/fml/relauncher/SideOnly;", "CLIENT")),
            ..ClassSpec::default()
        })];
        let out = aggregate(&classes, &mod_signals());
        assert_ne!(out.match_policy, Some(MatchPolicy::MustMatch));
    }

    #[test]
    fn undecided_mod_stays_unclassified() {
        // no annotations, no env, no content, no client surface: a common-code
        // library shape -- the classifier must refuse to guess
        let classes = vec![ci(
            "somelib/core/Util",
            &["net/minecraft/nbt/NBTTagCompound"],
            false,
        )];
        let out = aggregate(&classes, &mod_signals());
        assert_eq!(out.side, None);
        assert_eq!(out.match_policy, None);
        assert_eq!(out.kind, Some(JarKind::Mod), "identity makes it a mod");
    }

    // ── coremod / library kinds ─────────────────────────────────────────────

    #[test]
    fn loading_plugin_without_identity_is_a_coremod() {
        let classes = vec![ci_spec(&ClassSpec {
            this: "mymod/asm/CorePlugin",
            interfaces: &["net/minecraftforge/fml/relauncher/IFMLLoadingPlugin"],
            ..ClassSpec::default()
        })];
        let out = aggregate(&classes, &JarSignals::default());
        assert_eq!(out.kind, Some(JarKind::Coremod));
        assert_eq!(out.side, None);
        assert_eq!(out.match_policy, None);
    }

    #[test]
    fn manifest_coremod_and_mixin_configs_mark_a_coremod() {
        for signals in [
            JarSignals {
                manifest_coremod: true,
                ..JarSignals::default()
            },
            JarSignals {
                mixin_configs: 1,
                ..JarSignals::default()
            },
            JarSignals {
                manifest_tweaker: true,
                ..JarSignals::default()
            },
        ] {
            let out = aggregate(&[ci("some/asm/Transformer", &[], false)], &signals);
            assert_eq!(out.kind, Some(JarKind::Coremod));
        }
    }

    #[test]
    fn coremod_with_a_mod_identity_classifies_as_mod() {
        // mixinbooter-shape: FMLCorePlugin manifest + an mcmod.info identity
        let signals = JarSignals {
            manifest_coremod: true,
            meta_identity: true,
            ..JarSignals::default()
        };
        let out = aggregate(&[ci("zone/rong/Booter", &[], false)], &signals);
        assert_eq!(out.kind, Some(JarKind::Mod));
    }

    #[test]
    fn bare_class_jar_without_identity_is_a_library() {
        // ChickenASM-shape: classes, no identity, no coremod markers
        let out = aggregate(
            &[ci("codechicken/asm/Helper", &[], false)],
            &JarSignals::default(),
        );
        assert_eq!(out.kind, Some(JarKind::Library));
        assert_eq!(out.side, None);
    }

    // ── multi-mod jars ──────────────────────────────────────────────────────

    #[test]
    fn bundled_mods_take_the_worst_category() {
        // one bundled @Mod declares ARV "*", the other does not: strict wins
        let tolerant = ci_spec(&ClassSpec {
            this: "bundle/ModA",
            modid: Some("moda"),
            arv: Some("*"),
            ..ClassSpec::default()
        });
        let strict = ci_spec(&ClassSpec {
            this: "bundle/ModB",
            modid: Some("modb"),
            ..ClassSpec::default()
        });
        let content = ci_spec(&ClassSpec {
            this: "bundle/BlockThing",
            super_name: Some("net/minecraft/block/Block"),
            ..ClassSpec::default()
        });
        let out = aggregate(&[tolerant, strict, content], &mod_signals());
        assert_eq!(out.match_policy, Some(MatchPolicy::MustMatch));

        // both tolerant -> tolerant
        let t1 = ci_spec(&ClassSpec {
            this: "bundle/ModA",
            modid: Some("moda"),
            arv: Some("*"),
            ..ClassSpec::default()
        });
        let t2 = ci_spec(&ClassSpec {
            this: "bundle/ModB",
            modid: Some("modb"),
            arv: Some("*"),
            ..ClassSpec::default()
        });
        let out2 = aggregate(&[t1, t2], &mod_signals());
        assert_eq!(out2.match_policy, Some(MatchPolicy::Tolerant));
    }

    #[test]
    fn client_and_server_flagged_bundle_folds_to_both() {
        let client = ci_spec(&ClassSpec {
            this: "bundle/ClientPart",
            mod_sides: Some((true, false)),
            ..ClassSpec::default()
        });
        let server = ci_spec(&ClassSpec {
            this: "bundle/ServerPart",
            mod_sides: Some((false, true)),
            ..ClassSpec::default()
        });
        let out = aggregate(&[client, server], &mod_signals());
        // a client part and a server part make a both-sides jar; the policy
        // stays open (no content, no marker) and reports unclassified
        assert_eq!(out.side, Some(SideClass::Both));
        assert_eq!(out.match_policy, None);
    }

    // ── zip-level shells ────────────────────────────────────────────────────

    #[test]
    fn scan_jar_reads_classes_and_fabric_side() {
        let addon = build_class("ae2stuff/block/Foo", &["appeng/api/AEApi"], false, None);
        let fabric = br#"{"id":"mymod","environment":"client"}"#;
        let bytes = jar(&[
            ("ae2stuff/block/Foo.class", &addon),
            ("fabric.mod.json", fabric),
            ("pack.png", b"not a class"),
        ]);
        let out = scan_jar(&bytes);
        assert!(out.owned.contains("ae2stuff/block"));
        assert!(out.hard_refs.contains("appeng/api"));
        assert_eq!(out.side, Some(SideClass::Client));
        assert_eq!(out.kind, Some(JarKind::Mod), "fabric id is an identity");
    }

    #[test]
    fn scan_jar_detects_manifest_coremod_and_mixin_configs() {
        let cls = build_class("some/asm/Transformer", &[], false, None);
        let bytes = jar(&[
            ("some/asm/Transformer.class", &cls),
            (
                "META-INF/MANIFEST.MF",
                b"Manifest-Version: 1.0\nFMLCorePlugin: some.asm.Plugin\n",
            ),
            ("mixins.somemod.json", b"{}"),
        ]);
        let out = scan_jar(&bytes);
        assert_eq!(out.kind, Some(JarKind::Coremod));
    }

    #[test]
    fn mixin_config_names_match_both_conventions() {
        assert!(is_mixin_config_name("mixins.somemod.json"));
        assert!(is_mixin_config_name("somemod.mixins.json"));
        assert!(is_mixin_config_name("assets/x/other.mixins.json"));
        assert!(!is_mixin_config_name("mixins.json")); // bare, ambiguous
        assert!(!is_mixin_config_name("something.json"));
        assert!(!is_mixin_config_name("mixinsomething.json"));
    }

    #[test]
    fn scan_jar_tolerates_non_jar_bytes() {
        let out = scan_jar(b"not a zip at all");
        assert!(out.owned.is_empty() && out.hard_refs.is_empty());
        assert_eq!(out.side, None);
    }
}
