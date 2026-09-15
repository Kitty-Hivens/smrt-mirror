//! What changed between two pack configs, as rows a person can read.
//!
//! One diff, and every surface that says "what changed" reads it: the commit
//! box, the count beside it, the refusal a build answers with, and the record of
//! what a commit took in. They used to be three implementations -- a positional
//! JSON walk in the commit module, a row-level one in the panel, and a third
//! over built manifests -- so the number and the list under it could disagree,
//! and did: inserting a mod into an alphabetical list reported every row below
//! it as changed.
//!
//! Two rules make the answer match what a person did.
//!
//! **Rows are matched by identity, not by position.** A mod is its curator slug,
//! else the publisher project its pin names, else its filename -- the cascade
//! `domain::diff` already uses across two builds, so a re-pin reads as a re-pin
//! on both sides of the mirror. An asset is its destination path.
//!
//! **Only what a person can author is compared.** `owner` / `tier` /
//! `visibility` / `fork_of` are server-controlled, depfill-appended rows carry
//! `pulled`, and `display.requires` / `display.presence` are written by the fill
//! and the classifier on every save. Counting those is what made the old number
//! meaningless -- a save whose only effect was the fill writing two requires
//! lists reported 22 changes.
//!
//! The projection is the one `PackConfig::edit_rev` hashes, minus what only the
//! mirror writes inside a row: `edit_rev` still covers `display.requires`,
//! `display.presence` and the order rows sit in, and this does not. So an equal
//! revision means an empty diff, and the converse does not hold -- which is the
//! direction the cheap path in `commit_status` reads it, and the only direction
//! that is safe to.

use crate::domain::{DeclaredAsset, DeclaredMod, Display, PackConfig, SourceDecl};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;
use utoipa::ToSchema;

/// Which part of the config a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings/")]
pub enum ChangeGroup {
    Pack,
    Mods,
    Assets,
}

/// What happened to the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings/")]
pub enum ChangeOp {
    Add,
    Remove,
    Change,
}

/// What moved on a row that is in both configs. Named rather than inferred from
/// which of `from`/`to` are present: a view that has to guess renders "edited"
/// for everything it does not recognise, and "edited" is what the old list said
/// where it should have said "renamed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings/")]
pub enum ChangeField {
    /// The artifact it points at: a Modrinth version, a cache sha1, a static path.
    Pin,
    /// The install default a player gets.
    DefaultEnabled,
    /// Whether an asset installs unconditionally.
    Required,
    /// The name the launcher writes into `mods/`, the same artifact behind it.
    Filename,
    /// The curator-assigned stable identity (ADR 0002).
    Slug,
    /// The curator-written display block: name, description, category, license,
    /// url, icon, incompatibilities.
    Display,
    /// One of the pack's own fields.
    Value,
}

/// One difference between two configs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[ts(export, export_to = "bindings/")]
pub struct ConfigChange {
    pub group: ChangeGroup,
    pub op: ChangeOp,
    /// The row's identity within its group (`m:<project>`, `s:<slug>`,
    /// `f:<filename>`, an asset's `dest`, or a pack field's name). Stable across
    /// a re-pin and a rename, so a view can key a row by it and follow it.
    pub key: String,
    /// What to call the row on screen: a filename, an asset destination, or the
    /// pack field's own name.
    pub label: String,
    /// Which aspect moved; absent on an arrival or a departure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub field: Option<ChangeField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub to: Option<String>,
    /// The Modrinth project behind the row, when it has one. A pin is a version
    /// id on the wire (`sc43sMLj -> bqMxf6Ua`), which tells a reader nothing;
    /// the project is what lets a view fetch the version numbers and show those
    /// instead. Carried rather than resolved here: the diff must not wait on
    /// Modrinth to answer what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<String>,
}

/// The pack's own fields, in the order they are worth reading. Server-controlled
/// fields are absent by construction: a client cannot move them, so their moving
/// is never something a person did.
const PACK_FIELDS: &[&str] = &[
    "minecraft",
    "loader",
    "java",
    "version",
    "display_name",
    "tagline",
    "featured",
    "tags",
    "pack_meta",
    "auth",
];

/// Every difference between two configs, pack fields first, then mods, then
/// assets; arrivals, departures and moves in that order within each group.
pub fn diff_configs(before: &PackConfig, after: &PackConfig) -> Vec<ConfigChange> {
    let mut out = pack_rows(before, after);
    out.extend(sorted(mod_rows(before, after)));
    out.extend(sorted(asset_rows(before, after)));
    out
}

/// How far the live config has moved off a commit's snapshot.
///
/// `None` -- a pack that has never committed -- is one outstanding change, the
/// pack itself. A field count would read as noise on a pack that has simply
/// never used history.
pub fn uncommitted(head: Option<&PackConfig>, live: &PackConfig) -> usize {
    match head {
        None => 1,
        Some(head) => diff_configs(head, live).len(),
    }
}

/// A first commit read as change: it has no predecessor, so what it recorded is
/// everything it declared. The pack's own fields are not rows here -- against
/// nothing, a name and a loader are not a move -- but the mods and assets are,
/// which is what "what went in" means for the commit that started a pack.
pub fn initial(cfg: &PackConfig) -> Vec<ConfigChange> {
    let empty = PackConfig {
        mods: Vec::new(),
        assets: Vec::new(),
        ..cfg.clone()
    };
    diff_configs(&empty, cfg)
}

/// The whole config as one row, for when there is a checkpoint but its snapshot
/// cannot be read. Nothing can be compared, so nothing may be called clean: a
/// build must still refuse, and the count beside the list must still equal the
/// list.
pub fn whole_config() -> ConfigChange {
    ConfigChange {
        group: ChangeGroup::Pack,
        op: ChangeOp::Change,
        key: "config".into(),
        label: "config".into(),
        field: Some(ChangeField::Value),
        from: None,
        to: None,
        project: None,
    }
}

fn pack_rows(before: &PackConfig, after: &PackConfig) -> Vec<ConfigChange> {
    let mut values: BTreeMap<&str, (String, String)> = BTreeMap::new();
    let loader = |c: &PackConfig| format!("{} {}", c.loader.name, c.loader.version);
    values.insert(
        "minecraft",
        (
            before.minecraft_version.clone(),
            after.minecraft_version.clone(),
        ),
    );
    values.insert("loader", (loader(before), loader(after)));
    values.insert(
        "java",
        (before.java_major.to_string(), after.java_major.to_string()),
    );
    values.insert(
        "version",
        (
            before.version.clone().unwrap_or_default(),
            after.version.clone().unwrap_or_default(),
        ),
    );
    values.insert(
        "display_name",
        (before.display_name.clone(), after.display_name.clone()),
    );
    values.insert("tagline", (before.tagline.clone(), after.tagline.clone()));
    values.insert(
        "featured",
        (before.featured.to_string(), after.featured.to_string()),
    );
    values.insert("tags", (before.tags.join(", "), after.tags.join(", ")));

    let mut out = Vec::new();
    for name in PACK_FIELDS {
        match *name {
            // The pack card is a block of prose, urls and images, and the auth
            // precondition is a small record: which key inside them moved is not
            // worth a row apiece, and neither reads as a value on one line.
            "pack_meta" => {
                if opaque(&before.pack_meta) != opaque(&after.pack_meta) {
                    out.push(opaque_row("pack_meta"));
                }
            }
            "auth" => {
                if opaque(&before.auth) != opaque(&after.auth) {
                    out.push(opaque_row("auth"));
                }
            }
            field => {
                if let Some((from, to)) = values.get(field)
                    && from != to
                {
                    out.push(ConfigChange {
                        group: ChangeGroup::Pack,
                        op: ChangeOp::Change,
                        key: field.to_string(),
                        label: field.to_string(),
                        field: Some(ChangeField::Value),
                        from: Some(from.clone()),
                        to: Some(to.clone()),
                        project: None,
                    });
                }
            }
        }
    }
    out
}

fn opaque_row(field: &str) -> ConfigChange {
    ConfigChange {
        group: ChangeGroup::Pack,
        op: ChangeOp::Change,
        key: field.to_string(),
        label: field.to_string(),
        field: Some(ChangeField::Value),
        from: None,
        to: None,
        project: None,
    }
}

fn mod_rows(before: &PackConfig, after: &PackConfig) -> Vec<ConfigChange> {
    let was: BTreeMap<String, &DeclaredMod> = authored_mods(before);
    let now: BTreeMap<String, &DeclaredMod> = authored_mods(after);
    let mut out = Vec::new();
    for (key, m) in &now {
        if let Some(old) = was.get(key) {
            out.extend(mod_changes(key, old, m));
        }
    }

    // What identity could not carry, content can: a jar the mirror knows only by
    // its hash has no key beyond its filename, so renaming one leaves a
    // departure and an arrival pointing at the same artifact. Pairing them here,
    // before their changes are read, is what makes the rename a rename and still
    // reports whatever else moved on the same row.
    let gone: Vec<(&String, &&DeclaredMod)> = was
        .iter()
        .filter(|(key, _)| !now.contains_key(*key))
        .collect();
    let mut taken: std::collections::HashSet<&String> = std::collections::HashSet::new();
    for (key, old) in &gone {
        let pin_of = pin(&old.source);
        let paired = now.iter().find(|(k, m)| {
            !was.contains_key(*k) && !taken.contains(*k) && pin(&m.source) == pin_of
        });
        match paired {
            Some((new_key, new_mod)) => {
                taken.insert(new_key);
                out.extend(mod_changes(new_key, old, new_mod));
            }
            None => out.push(ConfigChange {
                group: ChangeGroup::Mods,
                op: ChangeOp::Remove,
                key: (*key).clone(),
                label: old.filename.clone(),
                field: None,
                from: Some(pin_of),
                to: None,
                project: project_of(&old.source),
            }),
        }
    }
    for (key, m) in &now {
        if was.contains_key(key) || taken.contains(key) {
            continue;
        }
        out.push(ConfigChange {
            group: ChangeGroup::Mods,
            op: ChangeOp::Add,
            key: key.clone(),
            label: m.filename.clone(),
            field: None,
            from: None,
            to: Some(pin(&m.source)),
            project: project_of(&m.source),
        });
    }
    out
}

/// Everything that moved on a mod present in both configs -- a row apiece,
/// because re-pinning a mod and switching off its install default are two
/// decisions, and a list that shows one of them is a list that lies about the
/// other.
fn mod_changes(key: &str, was: &DeclaredMod, now: &DeclaredMod) -> Vec<ConfigChange> {
    let row = |field: ChangeField, from: Option<String>, to: Option<String>| ConfigChange {
        group: ChangeGroup::Mods,
        op: ChangeOp::Change,
        key: key.to_string(),
        label: now.filename.clone(),
        field: Some(field),
        from,
        to,
        // Both sides of a matched row pin the same project by construction:
        // the project is what matched them.
        project: project_of(&now.source),
    };
    let mut out = Vec::new();
    let (from, to) = (pin(&was.source), pin(&now.source));
    if from != to {
        out.push(row(ChangeField::Pin, Some(from), Some(to)));
    } else if project_of(&was.source) != project_of(&now.source) {
        // The same version id under a different project: the pin reads
        // unchanged and the artifact is not the same one. Rare from the panel,
        // which writes both ids together, and silent without this.
        out.push(row(
            ChangeField::Pin,
            project_of(&was.source),
            project_of(&now.source),
        ));
    }
    if was.filename != now.filename {
        out.push(row(
            ChangeField::Filename,
            Some(was.filename.clone()),
            Some(now.filename.clone()),
        ));
    }
    if was.default_enabled != now.default_enabled {
        out.push(row(
            ChangeField::DefaultEnabled,
            Some(on_off(was.default_enabled)),
            Some(on_off(now.default_enabled)),
        ));
    }
    if was.slug != now.slug {
        out.push(row(ChangeField::Slug, was.slug.clone(), now.slug.clone()));
    }
    if authored_display(was.display.as_ref()) != authored_display(now.display.as_ref()) {
        out.push(row(ChangeField::Display, None, None));
    }
    out
}

fn asset_rows(before: &PackConfig, after: &PackConfig) -> Vec<ConfigChange> {
    let was: BTreeMap<&str, &DeclaredAsset> =
        before.assets.iter().map(|a| (a.dest.as_str(), a)).collect();
    let now: BTreeMap<&str, &DeclaredAsset> =
        after.assets.iter().map(|a| (a.dest.as_str(), a)).collect();
    let mut out = Vec::new();
    for (dest, a) in &now {
        if let Some(old) = was.get(dest) {
            out.extend(asset_changes(dest, old, a));
        }
    }

    // An asset installed to another path is the same file moved, on the same
    // evidence a renamed jar is: both sides point at one source.
    let gone: Vec<(&&str, &&DeclaredAsset)> = was
        .iter()
        .filter(|(dest, _)| !now.contains_key(*dest))
        .collect();
    let mut taken: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (dest, old) in &gone {
        let pin_of = pin(&old.source);
        let paired = now.iter().find(|(d, a)| {
            !was.contains_key(*d) && !taken.contains(*d) && pin(&a.source) == pin_of
        });
        match paired {
            Some((new_dest, new_asset)) => {
                taken.insert(new_dest);
                out.extend(asset_changes(new_dest, old, new_asset));
            }
            None => out.push(ConfigChange {
                group: ChangeGroup::Assets,
                op: ChangeOp::Remove,
                key: (**dest).to_string(),
                label: (**dest).to_string(),
                field: None,
                from: Some(pin_of),
                to: None,
                project: project_of(&old.source),
            }),
        }
    }
    for (dest, a) in &now {
        if was.contains_key(dest) || taken.contains(dest) {
            continue;
        }
        out.push(ConfigChange {
            group: ChangeGroup::Assets,
            op: ChangeOp::Add,
            key: (*dest).to_string(),
            label: (*dest).to_string(),
            field: None,
            from: None,
            to: Some(pin(&a.source)),
            project: project_of(&a.source),
        });
    }
    out
}

fn asset_changes(dest: &str, was: &DeclaredAsset, now: &DeclaredAsset) -> Vec<ConfigChange> {
    let row = |field: ChangeField, from: Option<String>, to: Option<String>| ConfigChange {
        group: ChangeGroup::Assets,
        op: ChangeOp::Change,
        key: dest.to_string(),
        label: dest.to_string(),
        field: Some(field),
        from,
        to,
        project: project_of(&now.source),
    };
    let mut out = Vec::new();
    let (from, to) = (pin(&was.source), pin(&now.source));
    if from != to {
        out.push(row(ChangeField::Pin, Some(from), Some(to)));
    }
    if was.dest != now.dest {
        out.push(row(
            ChangeField::Filename,
            Some(was.dest.clone()),
            Some(now.dest.clone()),
        ));
    }
    if was.required != now.required {
        out.push(row(
            ChangeField::Required,
            Some(on_off(was.required)),
            Some(on_off(now.required)),
        ));
    }
    if authored_display(was.display.as_ref()) != authored_display(now.display.as_ref()) {
        out.push(row(ChangeField::Display, None, None));
    }
    out
}

/// The declared mods a person authored, keyed by identity. Depfill's own rows
/// are dropped: the fill appends and prunes them on every save, and a library
/// arriving because a mod needs it is not a decision anyone made.
fn authored_mods(cfg: &PackConfig) -> BTreeMap<String, &DeclaredMod> {
    let mut out: BTreeMap<String, &DeclaredMod> = BTreeMap::new();
    for m in cfg.mods.iter().filter(|m| !m.pulled) {
        // A key two rows share is no key at all: the second would replace the
        // first, and every edit on the row that lost would be missing from the
        // diff -- which is the count the build gate trusts. A slug is typed by
        // hand, checked for uniqueness nowhere, and the editor offers the field
        // empty on every cached jar, so two rows keying alike is an ordinary
        // accident rather than a corrupt config. The filename is unique by
        // construction (`PackConfig::duplicate_declaration`), so it is what a
        // collision falls back to.
        let key = identity(m);
        let key = if out.contains_key(&key) {
            format!("f:{}", m.filename)
        } else {
            key
        };
        out.insert(key, m);
    }
    out
}

/// The identity a declared mod is matched by across two configs: the curator
/// slug (ADR 0002), else the publisher project a pin names, else the filename.
/// The same cascade `domain::diff::identity` uses across two builds, so one mod
/// reads as one mod wherever the mirror is asked.
///
/// The slug leads because it is the only one of the three that survives a move
/// between publishers, and that move is the whole point of having more than one
/// source type: a mod repinned off this mirror onto CurseForge is the same mod,
/// and a diff that reads it as a removal plus an addition says the pack lost
/// something it did not lose.
fn identity(m: &DeclaredMod) -> String {
    // A blank slug is the editor's own default for a cached jar, not an
    // identity anyone assigned: keying by it would make every jar nobody named
    // the same row.
    if let Some(s) = m.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return format!("s:{s}");
    }
    match &m.source {
        SourceDecl::Modrinth { project_id, .. } => format!("m:{project_id}"),
        SourceDecl::CurseForge { project_id, .. } => format!("cf:{project_id}"),
        // The tag is the version, so it is deliberately not part of this: a pin
        // moved to a newer release is the same mod.
        SourceDecl::Github { repo, asset, .. } => format!("gh:{repo}/{asset}"),
        SourceDecl::SmrtCache { .. } | SourceDecl::SmrtStatic { .. } => {
            format!("f:{}", m.filename)
        }
    }
}

/// What a source points at, as one comparable string. A static asset is its
/// path: the bytes behind it can change without the config moving at all, which
/// is a difference no config diff can see and the build's own preview does.
fn pin(source: &SourceDecl) -> String {
    match source {
        SourceDecl::Modrinth { version_id, .. } => version_id.clone(),
        SourceDecl::CurseForge { file_id, .. } => file_id.to_string(),
        SourceDecl::Github { tag, asset, .. } => format!("{tag}/{asset}"),
        SourceDecl::SmrtCache { sha1 } => sha1.clone(),
        SourceDecl::SmrtStatic { rel_path } => rel_path.clone(),
    }
}

fn project_of(source: &SourceDecl) -> Option<String> {
    match source {
        SourceDecl::Modrinth { project_id, .. } => Some(project_id.clone()),
        _ => None,
    }
}

/// A display block reduced to what a person wrote in it. `requires` is the
/// dependency fill's bookkeeping and `presence` is computed at build; an absent
/// block and one holding only those must compare equal, or the fill writing its
/// first requires list onto a mod reads as an edit someone made.
fn authored_display(display: Option<&Display>) -> String {
    let Some(d) = display else {
        return String::new();
    };
    let authored = Display {
        name: d.name.clone(),
        description: d.description.clone(),
        category: d.category.clone(),
        incompatible_with: d.incompatible_with.clone(),
        license: d.license.clone(),
        url: d.url.clone(),
        icon_url: d.icon_url.clone(),
        requires: Vec::new(),
        presence: None,
    };
    if authored.name.is_none()
        && authored.description.is_none()
        && authored.category.is_none()
        && authored.incompatible_with.is_empty()
        && authored.license.is_none()
        && authored.url.is_none()
        && authored.icon_url.is_none()
    {
        return String::new();
    }
    serde_json::to_string(&authored).unwrap_or_default()
}

fn opaque<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn on_off(on: bool) -> String {
    if on { "on" } else { "off" }.to_string()
}

/// Arrivals first, then departures, then moves; alphabetical inside each.
fn sorted(mut rows: Vec<ConfigChange>) -> Vec<ConfigChange> {
    let rank = |op: ChangeOp| match op {
        ChangeOp::Add => 0,
        ChangeOp::Remove => 1,
        ChangeOp::Change => 2,
    };
    rows.sort_by(|a, b| {
        rank(a.op)
            .cmp(&rank(b.op))
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LoaderSpec, PackMeta, PackTier, Visibility};

    fn cfg() -> PackConfig {
        PackConfig {
            pack_id: "Create".into(),
            display_name: "Create".into(),
            tagline: "Create-focused".into(),
            minecraft_version: "1.21.1".into(),
            loader: LoaderSpec {
                name: "neoforge".into(),
                version: "21.1.248".into(),
            },
            java_major: 21,
            version: Some("0.1".into()),
            tags: vec![],
            featured: false,
            mods: vec![
                modrinth_mod("Create.jar", "LNytGWDc", "sc43sMLj"),
                cache_mod("FTBLibrary.jar", "a".repeat(40)),
            ],
            assets: vec![DeclaredAsset {
                dest: "config/a.json".into(),
                required: true,
                source: SourceDecl::SmrtStatic {
                    rel_path: "config/a.json".into(),
                },
                display: None,
            }],
            auth: None,
            pack_meta: PackMeta::default(),
            owner: 211033194,
            tier: PackTier::Official,
            visibility: Visibility::Published,
            fork_of: None,
        }
    }

    fn modrinth_mod(filename: &str, project: &str, version: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::Modrinth {
                project_id: project.into(),
                version_id: version.into(),
            },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    fn cache_mod(filename: &str, sha1: String) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1 },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    #[test]
    fn an_unchanged_config_has_nothing_to_say() {
        assert!(diff_configs(&cfg(), &cfg()).is_empty());
    }

    #[test]
    fn a_mod_added_in_the_middle_is_one_change() {
        // the case the positional walk got wrong: an alphabetical list takes an
        // arrival in its middle, and every row below it shifts by one
        let mut after = cfg();
        after
            .mods
            .insert(1, modrinth_mod("Cosmetica.jar", "s9hF9QGp", "J4uskYvj"));
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].op, ChangeOp::Add);
        assert_eq!(rows[0].label, "Cosmetica.jar");
        assert_eq!(rows[0].project.as_deref(), Some("s9hF9QGp"));
    }

    #[test]
    fn an_added_or_removed_row_counts_once() {
        let mut after = cfg();
        after
            .mods
            .push(modrinth_mod("Sodium.jar", "AANobbMI", "HZAmZTNS"));
        assert_eq!(diff_configs(&cfg(), &after).len(), 1);
        assert_eq!(diff_configs(&after, &cfg()).len(), 1);
    }

    #[test]
    fn what_the_mirror_fills_in_is_not_a_change_anyone_made() {
        let mut after = cfg();
        after.mods[0].display = Some(Display {
            requires: vec![crate::domain::Requirement {
                filename: "FTBLibrary.jar".into(),
                version_range: None,
                optional: false,
            }],
            presence: Some(crate::domain::PresenceClass::Required),
            ..Display::default()
        });
        after.mods.push(DeclaredMod {
            pulled: true,
            ..cache_mod("Pulled.jar", "b".repeat(40))
        });
        assert!(diff_configs(&cfg(), &after).is_empty());
    }

    #[test]
    fn server_controlled_fields_are_never_a_change() {
        // publishing a pack, or a fork taking ownership, is not an edit anyone
        // has to check in -- the same projection edit_rev hashes
        let mut after = cfg();
        after.owner = 1;
        after.tier = PackTier::Community;
        after.visibility = Visibility::Draft;
        after.fork_of = Some("Industrial".into());
        assert!(diff_configs(&cfg(), &after).is_empty());
    }

    #[test]
    fn what_a_curator_writes_is_a_change() {
        let mut after = cfg();
        after.mods[0].display = Some(Display {
            url: Some("https://example.invalid/create".into()),
            ..Display::default()
        });
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Display));
    }

    #[test]
    fn a_moved_pin_carries_both_ends_and_its_project() {
        let mut after = cfg();
        after.mods[0].source = SourceDecl::Modrinth {
            project_id: "LNytGWDc".into(),
            version_id: "bqMxf6Ua".into(),
        };
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].field, Some(ChangeField::Pin));
        assert_eq!(rows[0].from.as_deref(), Some("sc43sMLj"));
        assert_eq!(rows[0].to.as_deref(), Some("bqMxf6Ua"));
        assert_eq!(rows[0].project.as_deref(), Some("LNytGWDc"));
    }

    #[test]
    fn a_renamed_jar_is_a_rename_not_a_swap() {
        // the same Modrinth project under a new filename: one row saying so,
        // where matching by filename would report a departure and an arrival
        let mut after = cfg();
        after.mods[0].filename = "Create-6.0.jar".into();
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Filename));
        assert_eq!(rows[0].from.as_deref(), Some("Create.jar"));
        assert_eq!(rows[0].to.as_deref(), Some("Create-6.0.jar"));
    }

    #[test]
    fn a_re_pin_and_a_toggle_are_two_changes() {
        let mut after = cfg();
        after.mods[0].source = SourceDecl::Modrinth {
            project_id: "LNytGWDc".into(),
            version_id: "bqMxf6Ua".into(),
        };
        after.mods[0].default_enabled = false;
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Pin));
        assert_eq!(rows[1].field, Some(ChangeField::DefaultEnabled));
        assert_eq!(rows[1].to.as_deref(), Some("off"));
    }

    #[test]
    fn a_cache_jar_renamed_is_a_rename_not_a_swap() {
        // no Modrinth project and no slug, so identity is the filename -- and
        // the two rows still point at one artifact, which is what says so
        let mut after = cfg();
        after.mods[1].filename = "FTBLibrary-2110.jar".into();
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].op, ChangeOp::Change);
        assert_eq!(rows[0].field, Some(ChangeField::Filename));
        assert_eq!(rows[0].from.as_deref(), Some("FTBLibrary.jar"));
        assert_eq!(rows[0].to.as_deref(), Some("FTBLibrary-2110.jar"));
    }

    #[test]
    fn two_rows_sharing_a_slug_are_two_rows() {
        // a slug is typed by hand and unique nowhere; keying two jars alike
        // would drop one of them out of the diff, and the count with it
        let mut base = cfg();
        base.mods[1].slug = Some("lib".into());
        base.mods.push(DeclaredMod {
            slug: Some("lib".into()),
            ..cache_mod("Other.jar", "e".repeat(40))
        });
        let mut after = base.clone();
        after.mods[1].default_enabled = false;
        let rows = diff_configs(&base, &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "FTBLibrary.jar");
        assert_eq!(rows[0].field, Some(ChangeField::DefaultEnabled));
    }

    #[test]
    fn a_blank_slug_is_not_an_identity() {
        // the editor offers the field empty on every cached jar, so two blank
        // ones must not read as one row
        let mut base = cfg();
        base.mods[1].slug = Some("  ".into());
        base.mods.push(DeclaredMod {
            slug: Some(String::new()),
            ..cache_mod("Other.jar", "e".repeat(40))
        });
        let mut after = base.clone();
        after.mods[2].default_enabled = false;
        let rows = diff_configs(&base, &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "Other.jar");
    }

    #[test]
    fn a_row_repointed_at_another_project_is_a_change() {
        // same version id, different project: the pin reads unchanged and the
        // artifact is not the same one
        let mut after = cfg();
        after.mods[0].source = SourceDecl::Modrinth {
            project_id: "AANobbMI".into(),
            version_id: "sc43sMLj".into(),
        };
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Pin));
        assert_eq!(rows[0].from.as_deref(), Some("LNytGWDc"));
        assert_eq!(rows[0].to.as_deref(), Some("AANobbMI"));
    }

    #[test]
    fn a_rename_still_reports_what_else_moved_on_the_row() {
        // pairing a departure with an arrival must not swallow the rest of the
        // row: renaming a jar and switching it off is two decisions
        let mut after = cfg();
        after.mods[1].filename = "FTBLibrary-2110.jar".into();
        after.mods[1].default_enabled = false;
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Filename));
        assert_eq!(rows[1].field, Some(ChangeField::DefaultEnabled));
        assert_eq!(rows[1].to.as_deref(), Some("off"));
    }

    #[test]
    fn a_moved_asset_reports_the_move_and_the_rest() {
        let mut after = cfg();
        after.assets[0].dest = "config/b.json".into();
        after.assets[0].required = false;
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Filename));
        assert_eq!(rows[1].field, Some(ChangeField::Required));
    }

    #[test]
    fn a_different_jar_under_a_new_name_is_still_two_rows() {
        // nothing ties them together: a departure and an arrival is the honest
        // reading, and calling it a rename would invent a relationship
        let mut after = cfg();
        after.mods[1] = cache_mod("Something.jar", "d".repeat(40));
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].op, ChangeOp::Add);
        assert_eq!(rows[1].op, ChangeOp::Remove);
    }

    #[test]
    fn an_asset_moved_to_another_path_is_a_move() {
        let mut after = cfg();
        after.assets[0].dest = "config/b.json".into();
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Filename));
        assert_eq!(rows[0].from.as_deref(), Some("config/a.json"));
    }

    #[test]
    fn a_cache_jar_replaced_under_the_same_name_is_a_re_pin() {
        let mut after = cfg();
        after.mods[1].source = SourceDecl::SmrtCache {
            sha1: "c".repeat(40),
        };
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].field, Some(ChangeField::Pin));
        assert_eq!(rows[0].label, "FTBLibrary.jar");
    }

    #[test]
    fn the_loader_reads_as_a_loader_not_as_two_fields() {
        let mut after = cfg();
        after.loader = LoaderSpec {
            name: "neoforge".into(),
            version: "21.1.250".into(),
        };
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "loader");
        assert_eq!(rows[0].from.as_deref(), Some("neoforge 21.1.248"));
        assert_eq!(rows[0].to.as_deref(), Some("neoforge 21.1.250"));
    }

    #[test]
    fn an_asset_moves_by_its_destination() {
        let mut after = cfg();
        after.assets[0].required = false;
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].group, ChangeGroup::Assets);
        assert_eq!(rows[0].field, Some(ChangeField::Required));
    }

    #[test]
    fn a_pack_with_no_history_has_one_outstanding_change() {
        assert_eq!(uncommitted(None, &cfg()), 1);
        assert_eq!(uncommitted(Some(&cfg()), &cfg()), 0);
    }

    #[test]
    fn groups_are_ordered_pack_then_mods_then_assets() {
        let mut after = cfg();
        after.tagline = "Create, heavier".into();
        after
            .mods
            .push(modrinth_mod("Sodium.jar", "AANobbMI", "HZ"));
        after.assets[0].required = false;
        let rows = diff_configs(&cfg(), &after);
        assert_eq!(
            rows.iter().map(|r| r.group).collect::<Vec<_>>(),
            vec![ChangeGroup::Pack, ChangeGroup::Mods, ChangeGroup::Assets]
        );
    }
}
