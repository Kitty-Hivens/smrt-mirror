//! A pack's config, edited by more than one person at once.
//!
//! #52 stopped a save from overwriting someone else's and #113 made the other
//! editors visible, but the unit of conflict was still the whole config: two
//! people who touched different things collided anyway, and one of them
//! reapplied by hand.
//!
//! So the config becomes a document. Every editor's changes reach it, it merges
//! them by construction, and the result is written back to `config.json` --
//! which stays exactly what it was, so the build, the publish check and the CLI
//! go on reading a plain file and never learn any of this happened.
//!
//! The unit of a change is the shape of the thing changed, which is the part
//! worth getting right. A paragraph merges a character at a time, because two
//! people writing in one sentence have no correct winner. A mod row merges as a
//! row: rows added by either side both land, and watching a neighbour's
//! filename appear one letter at a time is noise rather than collaboration. A
//! scalar -- the loader, the Minecraft version -- has nothing to merge inside
//! it, so the last write wins and the panel is left to say who moved it.
//!
//! The mapping is generic rather than field by field. The config travels through
//! its own JSON shape: objects become maps, arrays become arrays, and a short
//! list of paths says which strings are prose. A field added to `PackConfig`
//! later syncs and materialises without anyone remembering to teach this file
//! about it -- a hand-written mapping of a struct this size is a list of places
//! to forget.
//!
//! Deliberately not persisted as operations. The document is a live merge point
//! rebuilt from the stored config whenever nobody holds it, so a restart costs
//! the history of how the content got there and none of the content. Keeping
//! the ops is what a commit log is for (#122), and it should arrive with the
//! decisions that belong to it rather than as a side effect of merging.

use crate::domain::PackConfig;
use anyhow::{Context, Result, bail};
use serde_json::{Map as JsonMap, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use yrs::updates::decoder::Decode;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, Doc, GetString, In, Map, MapPrelim, MapRef, Out, ReadTxn,
    TextPrelim, Transact, Update,
};

/// The document's one root. A single named map rather than a root per field:
/// the config is one object, and its shape is the map's shape.
const ROOT: &str = "config";

/// Fields the server owns. They are never in the document, so a client cannot
/// propose a change to one even by accident -- the rule the config PUT already
/// enforces, kept here by construction rather than by carrying values back over
/// afterwards.
const SERVER_OWNED: [&str; 4] = ["owner", "tier", "visibility", "fork_of"];

/// Which strings are prose, by their path in the config. Everything else is a
/// value replaced whole.
///
/// The list is short on purpose. Prose is what a person composes, and where two
/// people in one paragraph both need their words to survive. A filename, a
/// version, a URL is a value: it is finished or it is not, and streaming it
/// half-typed to everyone in the room tells them nothing.
fn is_prose(path: &str) -> bool {
    matches!(path, "tagline" | "pack_meta.description_md")
        // The same two fields written in another language (#195). A translation
        // is composed the same way the original is, by the same people at the
        // same time, so it merges the same way. The tag is part of the path, so
        // this is a prefix rather than a list nobody can write in advance.
        || path.starts_with("pack_meta.tagline_i18n.")
        || path.starts_with("pack_meta.description_md_i18n.")
}

/// One pack's live document.
pub struct PackDoc {
    doc: Doc,
    root: MapRef,
}

impl PackDoc {
    /// A document seeded from what is on disk, in one transaction, so every
    /// client that syncs afterwards sees the same origin.
    pub fn from_config(cfg: &PackConfig) -> Result<Self> {
        let doc = Doc::new();
        let root = doc.get_or_insert_map(ROOT);
        let mut value = serde_json::to_value(cfg).context("config as JSON")?;
        let obj = value
            .as_object_mut()
            .context("a config serializes to an object")?;
        for key in SERVER_OWNED {
            obj.remove(key);
        }
        {
            let mut txn = doc.transact_mut();
            for (key, child) in obj.iter() {
                root.insert(&mut txn, key.as_str(), input(key, child));
            }
        }
        Ok(PackDoc { doc, root })
    }

    /// An empty document, to be filled by applying someone else's state.
    ///
    /// This is how every client must join, and the distinction is not academic:
    /// a joiner that seeds itself from the config and *then* applies the
    /// mirror's state has written its own insertions for the same keys, so the
    /// two sets are concurrent and every scalar becomes a coin toss between
    /// them. Join empty; the state carries everything.
    pub fn empty() -> Self {
        let doc = Doc::new();
        let root = doc.get_or_insert_map(ROOT);
        PackDoc { doc, root }
    }

    /// Everything the document knows, as one update: what a joining editor
    /// applies to catch up. A config is small enough that asking for the whole
    /// of it costs less than negotiating what is missing.
    pub fn state(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default())
    }

    /// Merge someone's edit in.
    pub fn apply(&self, update: &[u8]) -> Result<()> {
        let update = Update::decode_v1(update).context("decoding a document update")?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .context("applying a document update")?;
        Ok(())
    }

    /// The document as a config, with the server-owned fields taken from `base`
    /// -- they were never in the document to be changed.
    ///
    /// Fails rather than guesses when the merged document is not a config. Two
    /// people can leave it briefly nonsensical, and the honest answer is to say
    /// so and keep the last good file, not to write half of one.
    pub fn to_config(&self, base: &PackConfig) -> Result<PackConfig> {
        let mut obj = {
            let txn = self.doc.transact();
            match map_value(&self.root, &txn) {
                Value::Object(obj) => obj,
                _ => bail!("the document root is not an object"),
            }
        };
        let mut server = serde_json::to_value(base).context("base config as JSON")?;
        let server = server
            .as_object_mut()
            .context("a config serializes to an object")?;
        for key in SERVER_OWNED {
            if let Some(v) = server.remove(key) {
                obj.insert(key.to_string(), v);
            }
        }
        serde_json::from_value(Value::Object(obj)).context("the merged document is not a config")
    }
}

// ── the config's JSON shape, in and out of the document ─────────────────────

/// `path` is the dotted path of the value being written, which is what decides
/// whether a string is prose. Array elements keep their array's path: prose is a
/// property of a field, and a list of them is that field repeated.
fn input(path: &str, value: &Value) -> In {
    match value {
        Value::Object(obj) => In::Map(MapPrelim::from_iter(
            obj.iter()
                .map(|(k, v)| (k.clone(), input(&child_path(path, k), v))),
        )),
        Value::Array(items) => In::Array(ArrayPrelim::from(
            items.iter().map(|v| input(path, v)).collect::<Vec<_>>(),
        )),
        Value::String(s) if is_prose(path) => In::Text(TextPrelim::new(s.clone()).into()),
        Value::String(s) => In::Any(Any::String(s.as_str().into())),
        Value::Bool(b) => In::Any(Any::Bool(*b)),
        // Every number goes in as a double, including the whole ones.
        //
        // The other half of this panel reads the same config over REST, where a
        // browser's JSON.parse gives it doubles -- so the document has to give
        // doubles too, or the two halves disagree about the same field. Yjs
        // decodes yrs's BigInt as a JavaScript BigInt, which is a value JSON
        // cannot serialize at all: the editor merged an update and then threw on
        // the next save, with the config's own owner id as the poison.
        //
        // Nothing a pack config carries comes near the range where a double
        // stops being exact -- the largest is a GitHub uid.
        Value::Number(n) => In::Any(Any::Number(n.as_f64().unwrap_or_default())),
        Value::Null => In::Any(Any::Null),
    }
}

fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn map_value<T: ReadTxn>(map: &MapRef, txn: &T) -> Value {
    let mut obj = JsonMap::new();
    for (key, out) in map.iter(txn) {
        obj.insert(key.to_string(), out_value(&out, txn));
    }
    Value::Object(obj)
}

fn array_value<T: ReadTxn>(array: &ArrayRef, txn: &T) -> Value {
    Value::Array(array.iter(txn).map(|out| out_value(&out, txn)).collect())
}

fn out_value<T: ReadTxn>(out: &Out, txn: &T) -> Value {
    match out {
        Out::Any(any) => any_value(any),
        Out::YText(text) => Value::String(text.get_string(txn)),
        Out::YArray(array) => array_value(array, txn),
        Out::YMap(map) => map_value(map, txn),
        // Nothing here writes one; a document carrying one is not this document.
        _ => Value::Null,
    }
}

/// The integer a double exactly is, when it is one. `None` for a fraction, an
/// infinity, a NaN, or a magnitude past where a double still counts by ones.
fn whole(n: f64) -> Option<i64> {
    (n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0).then_some(n as i64)
}

fn any_value(any: &Any) -> Value {
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        // A whole double comes back as a whole number, not as `21.0`. The value
        // is on its way into a `PackConfig`, where `java_major` is a u32 and a
        // float refuses to deserialize -- so a round trip through the document
        // would fail on a field nobody touched.
        Any::Number(n) => whole(*n)
            .map(|i| Value::Number(i.into()))
            .or_else(|| serde_json::Number::from_f64(*n).map(Value::Number))
            .unwrap_or(Value::Null),
        // Still read: documents this process is already holding were seeded
        // before numbers went in as doubles, and they outlive the change.
        Any::BigInt(i) => Value::Number((*i).into()),
        Any::String(s) => Value::String(s.to_string()),
        Any::Array(items) => Value::Array(items.iter().map(any_value).collect()),
        Any::Map(entries) => Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), any_value(v)))
                .collect(),
        ),
        Any::Buffer(_) => Value::Null,
    }
}

/// Every pack anyone is editing. A document appears when someone asks for it
/// and stays for the lifetime of the process: it is a few kilobytes, and
/// dropping one while an editor still holds its own copy would strand them on a
/// history the mirror no longer shares.
#[derive(Default)]
pub struct PackDocs {
    docs: Mutex<HashMap<String, Arc<PackDoc>>>,
    /// The last ticket handed out per pack, for the wait before writing to
    /// disk. A counter rather than a timer handle: whoever still holds the
    /// latest ticket when the wait is over does the write, and every earlier
    /// one finds it has been superseded and goes home.
    tickets: Mutex<HashMap<String, u64>>,
}

impl PackDocs {
    /// The document for a pack, seeded from `cfg` if this is the first ask.
    pub fn get_or_seed(&self, pack_id: &str, cfg: &PackConfig) -> Result<Arc<PackDoc>> {
        let mut docs = self.docs.lock().unwrap();
        if let Some(doc) = docs.get(pack_id) {
            return Ok(doc.clone());
        }
        let doc = Arc::new(PackDoc::from_config(cfg)?);
        docs.insert(pack_id.to_string(), doc.clone());
        Ok(doc)
    }

    /// The document for a pack, if one is live. `None` means nobody is editing
    /// it and the config on disk is the whole truth.
    pub fn get(&self, pack_id: &str) -> Option<Arc<PackDoc>> {
        self.docs.lock().unwrap().get(pack_id).cloned()
    }

    /// Take a ticket for the write that should follow this edit.
    pub fn touch(&self, pack_id: &str) -> u64 {
        let mut tickets = self.tickets.lock().unwrap();
        let n = tickets.entry(pack_id.to_string()).or_insert(0);
        *n += 1;
        *n
    }

    /// Is this still the newest edit, or has someone typed since?
    pub fn is_current(&self, pack_id: &str, ticket: u64) -> bool {
        self.tickets.lock().unwrap().get(pack_id) == Some(&ticket)
    }

    /// Forget a pack's document, so the next editor is seeded from disk again.
    /// For writes that go around the editor -- a revert, a CLI run -- which the
    /// document would otherwise keep overwriting with what it still remembers.
    pub fn forget(&self, pack_id: &str) {
        self.docs.lock().unwrap().remove(pack_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        DeclaredMod, Display, LoaderSpec, PackMeta, PackTier, SourceDecl, Visibility,
    };
    // only the tests type into the document; the mirror only merges what arrives
    use yrs::Text;

    fn cache_mod(filename: &str, sha1: &str) -> DeclaredMod {
        DeclaredMod {
            filename: filename.into(),
            default_enabled: true,
            source: SourceDecl::SmrtCache { sha1: sha1.into() },
            display: None,
            slug: None,
            pulled: false,
        }
    }

    fn config(mods: Vec<DeclaredMod>) -> PackConfig {
        PackConfig {
            pack_id: "Industrial".into(),
            display_name: "Industrial".into(),
            tagline: "Heavy tech".into(),
            minecraft_version: "1.12.2".into(),
            loader: LoaderSpec {
                name: "forge".into(),
                version: "14.23.5.2860".into(),
            },
            java_major: 8,
            version: Some("0.4".into()),
            tags: vec!["tech".into()],
            featured: true,
            mods,
            assets: vec![],
            auth: None,
            pack_meta: PackMeta {
                description_md: Some("A pack.".into()),
                ..Default::default()
            },
            owner: 211033194,
            tier: PackTier::Community,
            visibility: Visibility::Draft,
            fork_of: Some("Create".into()),
        }
    }

    /// A second editor, caught up from the mirror's copy the way the panel is on
    /// open.
    fn joined(server: &PackDoc) -> PackDoc {
        let doc = PackDoc::empty();
        doc.apply(&server.state()).unwrap();
        doc
    }

    fn push_mod(doc: &PackDoc, m: &DeclaredMod) {
        let mut txn = doc.doc.transact_mut();
        let Some(Out::YArray(mods)) = doc.root.get(&txn, "mods") else {
            panic!("the document carries a mods array");
        };
        let len = mods.len(&txn);
        mods.insert(
            &mut txn,
            len,
            input("mods", &serde_json::to_value(m).unwrap()),
        );
    }

    fn set(doc: &PackDoc, key: &str, value: Value) {
        let mut txn = doc.doc.transact_mut();
        doc.root.insert(&mut txn, key, input(key, &value));
    }

    fn sync(from: &PackDoc, to: &PackDoc) {
        to.apply(&from.state()).unwrap();
    }

    fn back(doc: &PackDoc) -> PackConfig {
        doc.to_config(&config(vec![])).unwrap()
    }

    // The whole config survives the trip, field for field. Everything else here
    // is about merging; this is about the mapping being faithful in the first
    // place, since merging a lossy projection is worse than not merging.
    #[test]
    fn a_config_round_trips_through_the_document() {
        let cfg = config(vec![
            cache_mod("jei.jar", &"a".repeat(40)),
            DeclaredMod {
                display: Some(Display {
                    name: Some("Applied Energistics".into()),
                    category: Some("tech".into()),
                    ..Display::default()
                }),
                slug: Some("ae2".into()),
                ..cache_mod("ae2.jar", &"b".repeat(40))
            },
        ]);
        let doc = PackDoc::from_config(&cfg).unwrap();
        let out = doc.to_config(&cfg).unwrap();
        assert_eq!(
            serde_json::to_value(&out).unwrap(),
            serde_json::to_value(&cfg).unwrap()
        );
    }

    // The server's fields are not in the document at all, so there is nothing
    // for a client to change: publishing a pack, or forking one, is not an edit.
    #[test]
    fn the_server_owns_its_fields_and_the_document_never_sees_them() {
        let cfg = config(vec![]);
        let doc = PackDoc::from_config(&cfg).unwrap();
        {
            let txn = doc.doc.transact();
            for key in SERVER_OWNED {
                assert!(
                    doc.root.get(&txn, key).is_none(),
                    "{key} must not be in the document"
                );
            }
        }
        let mut base = cfg.clone();
        base.visibility = Visibility::Published;
        base.owner = 42;
        let out = doc.to_config(&base).unwrap();
        assert_eq!(out.visibility, Visibility::Published);
        assert_eq!(out.owner, 42);
    }

    // The collision the whole thing exists for: two people adding a mod each,
    // neither having seen the other. Today one of them is refused and reapplies
    // by hand.
    #[test]
    fn two_people_adding_a_mod_each_both_land() {
        let server =
            PackDoc::from_config(&config(vec![cache_mod("jei.jar", &"a".repeat(40))])).unwrap();
        let ada = joined(&server);
        let bo = joined(&server);

        push_mod(&ada, &cache_mod("ae2.jar", &"b".repeat(40)));
        push_mod(&bo, &cache_mod("thermal.jar", &"c".repeat(40)));

        sync(&ada, &server);
        sync(&bo, &server);

        let names: Vec<String> = back(&server).mods.into_iter().map(|m| m.filename).collect();
        assert_eq!(names.len(), 3, "the original and both additions: {names:?}");
        for expected in ["jei.jar", "ae2.jar", "thermal.jar"] {
            assert!(
                names.contains(&expected.to_string()),
                "{expected} in {names:?}"
            );
        }
    }

    // Two people in one paragraph, at the same time, and neither loses a word.
    // A revision check has no correct answer here: whichever save it refuses,
    // someone retypes a sentence.
    #[test]
    fn two_people_writing_in_one_paragraph_both_keep_their_words() {
        let server = PackDoc::from_config(&config(vec![])).unwrap();
        let ada = joined(&server);
        let bo = joined(&server);

        let insert = |doc: &PackDoc, at_end: bool, text: &str| {
            let mut txn = doc.doc.transact_mut();
            let Some(Out::YMap(meta)) = doc.root.get(&txn, "pack_meta") else {
                panic!("pack_meta is a map");
            };
            let Some(Out::YText(description)) = meta.get(&txn, "description_md") else {
                panic!("the description is prose");
            };
            let at = if at_end {
                description.get_string(&txn).len() as u32
            } else {
                0
            };
            description.insert(&mut txn, at, text);
        };
        insert(&ada, true, " Heavy tech.");
        insert(&bo, false, "The ");

        sync(&ada, &server);
        sync(&bo, &server);

        let description = back(&server).pack_meta.description_md.expect("still there");
        assert!(
            description.contains("Heavy tech."),
            "Ada's words survive: {description}"
        );
        assert!(
            description.starts_with("The "),
            "and Bo's, where he put them: {description}"
        );
        assert!(
            description.contains("A pack."),
            "and what was there already: {description}"
        );
    }

    // A translation is prose for the same reason the original is (#195): the
    // card is written once per language, by the same people, in the same
    // session. Replaced whole it would be last-writer-wins against a colleague
    // mid-sentence -- and the language nobody else in the room reads is exactly
    // where a silently overwritten paragraph goes unnoticed.
    #[test]
    fn two_people_writing_one_translation_both_keep_their_words() {
        let mut seed = config(vec![]);
        seed.pack_meta.description_md_i18n = Some(std::collections::BTreeMap::from([(
            "ru".to_string(),
            "Пак.".to_string(),
        )]));
        let server = PackDoc::from_config(&seed).unwrap();
        let ada = joined(&server);
        let bo = joined(&server);

        let russian = |doc: &PackDoc, write: &dyn Fn(&yrs::TextRef, &mut yrs::TransactionMut)| {
            let mut txn = doc.doc.transact_mut();
            let Some(Out::YMap(meta)) = doc.root.get(&txn, "pack_meta") else {
                panic!("pack_meta is a map");
            };
            let Some(Out::YMap(by_language)) = meta.get(&txn, "description_md_i18n") else {
                panic!("the translations are a map");
            };
            let Some(Out::YText(ru)) = by_language.get(&txn, "ru") else {
                panic!("a translation is prose");
            };
            write(&ru, &mut txn);
        };
        russian(&ada, &|ru, txn| ru.push(txn, " Тяжёлый."));
        russian(&bo, &|ru, txn| ru.insert(txn, 0, "Этот "));

        sync(&ada, &server);
        sync(&bo, &server);

        let description = back(&server)
            .pack_meta
            .description_md_i18n
            .and_then(|m| m.get("ru").cloned())
            .expect("still there");
        assert!(
            description.contains("Тяжёлый."),
            "Ada's words survive: {description}"
        );
        assert!(
            description.starts_with("Этот "),
            "and Bo's, where he put them: {description}"
        );
        assert!(
            description.contains("Пак."),
            "and what was there already: {description}"
        );
    }

    // The panel merged an update and then threw on the next save: yjs decodes
    // yrs's BigInt as a JavaScript BigInt, and JSON.stringify cannot serialize
    // one at all. Every whole number in a config went in as a BigInt, so the
    // pack's own owner id was enough to poison the editor.
    #[test]
    fn no_number_reaches_the_browser_as_a_bigint() {
        let m = DeclaredMod {
            filename: "Sable.jar".into(),
            ..serde_json::from_value(serde_json::json!({
                "filename": "Sable.jar",
                "source": {"type": "smrt_cache", "sha1": "a".repeat(40)}
            }))
            .unwrap()
        };
        let doc = PackDoc::from_config(&config(vec![m])).unwrap();
        let txn = doc.doc.transact();

        let mut found = Vec::new();
        for (key, out) in doc.root.iter(&txn) {
            walk(key, &out, &txn, &mut found);
        }
        assert!(
            found.is_empty(),
            "these arrive as BigInt and break JSON.stringify in the panel: {found:?}"
        );

        fn walk<T: ReadTxn>(at: &str, out: &Out, txn: &T, found: &mut Vec<String>) {
            match out {
                Out::Any(Any::BigInt(_)) => found.push(at.to_string()),
                Out::Any(Any::Array(items)) => {
                    if items.iter().any(|a| matches!(a, Any::BigInt(_))) {
                        found.push(at.to_string());
                    }
                }
                Out::YMap(map) => {
                    for (key, out) in map.iter(txn) {
                        walk(&format!("{at}.{key}"), &out, txn, found);
                    }
                }
                Out::YArray(array) => {
                    for (i, out) in array.iter(txn).enumerate() {
                        walk(&format!("{at}.{i}"), &out, txn, found);
                    }
                }
                _ => {}
            }
        }
    }

    // A whole double must come back a whole number: the value is on its way into
    // a PackConfig, where java_major is a u32 and `21.0` refuses to deserialize.
    #[test]
    fn whole_numbers_survive_the_round_trip_as_integers() {
        let doc = PackDoc::from_config(&config(vec![])).unwrap();
        let out = back(&doc);
        assert_eq!(out.java_major, config(vec![]).java_major);
        assert_eq!(out.owner, config(vec![]).owner);

        assert_eq!(whole(21.0), Some(21));
        assert_eq!(whole(211033194.0), Some(211033194));
        assert_eq!(whole(0.5), None);
        assert_eq!(whole(f64::NAN), None);
        assert_eq!(whole(f64::INFINITY), None);
    }

    // Different scalars are not a collision at all -- the everyday case the
    // whole-config revision refused anyway.
    #[test]
    fn two_people_changing_different_scalars_both_apply() {
        let server = PackDoc::from_config(&config(vec![])).unwrap();
        let ada = joined(&server);
        let bo = joined(&server);

        set(&ada, "display_name", Value::String("Industrial II".into()));
        set(&bo, "java_major", Value::Number(21.into()));

        sync(&ada, &server);
        sync(&bo, &server);

        let out = back(&server);
        assert_eq!(out.display_name, "Industrial II");
        assert_eq!(out.java_major, 21);
    }

    // The same scalar, changed by both. There is no correct merge inside one
    // value, so one wins -- but it must be one of the two, the same one for
    // everybody, and the result must still be a config.
    #[test]
    fn the_same_scalar_changed_by_both_settles_on_one_of_them() {
        let server = PackDoc::from_config(&config(vec![])).unwrap();
        let ada = joined(&server);
        let bo = joined(&server);

        set(&ada, "minecraft_version", Value::String("1.20.1".into()));
        set(&bo, "minecraft_version", Value::String("1.21.1".into()));

        sync(&ada, &server);
        sync(&bo, &server);
        // the editors converge on the mirror's answer, not on their own
        sync(&server, &ada);
        sync(&server, &bo);

        let winner = back(&server).minecraft_version;
        assert!(
            winner == "1.20.1" || winner == "1.21.1",
            "one of the two, not a blend: {winner}"
        );
        assert_eq!(back(&ada).minecraft_version, winner);
        assert_eq!(back(&bo).minecraft_version, winner);
    }

    // The trap the join API exists to close. A client that seeds itself from the
    // config and then applies the mirror's state has authored its own value for
    // every key, concurrently with the mirror's -- and a map key resolves to one
    // value, so one whole `mods` array replaces the other along with everything
    // in it. Not a duplicate: a silent loss, decided by whichever random client
    // id sorts higher. Nothing in the types says join empty, so this does.
    #[test]
    fn joining_empty_is_what_makes_the_merge_deterministic() {
        let server =
            PackDoc::from_config(&config(vec![cache_mod("jei.jar", &"a".repeat(40))])).unwrap();

        let correct = PackDoc::empty();
        correct.apply(&server.state()).unwrap();
        let mods = back(&correct).mods;
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].filename, "jei.jar", "exactly what the mirror holds");

        // seeded with a different pack, so whichever side wins, the other's mod
        // is gone -- which is the point, and is true without depending on who won
        let wrong =
            PackDoc::from_config(&config(vec![cache_mod("ae2.jar", &"b".repeat(40))])).unwrap();
        wrong.apply(&server.state()).unwrap();
        let names: Vec<String> = back(&wrong).mods.into_iter().map(|m| m.filename).collect();
        assert_eq!(
            names.len(),
            1,
            "one array replaced the other whole, rather than merging: {names:?}"
        );
    }

    // The wait before writing to disk. Every keystroke asks for a write; only
    // the last one may perform it, or a sentence becomes forty file writes and
    // forty revision bumps, each of which would refuse somebody else's save.
    #[test]
    fn only_the_newest_edit_writes() {
        let docs = PackDocs::default();
        let first = docs.touch("Industrial");
        assert!(
            docs.is_current("Industrial", first),
            "nothing has followed it"
        );

        let second = docs.touch("Industrial");
        assert!(!docs.is_current("Industrial", first), "superseded");
        assert!(docs.is_current("Industrial", second));

        // packs wait on their own
        let other = docs.touch("Create");
        assert!(docs.is_current("Create", other));
        assert!(docs.is_current("Industrial", second));
    }

    // Applying the same update twice must not double anything. Clients resend
    // when a response is lost, and a merge that counts the retry is worse than
    // one that refuses it.
    #[test]
    fn the_same_edit_arriving_twice_lands_once() {
        let server = PackDoc::from_config(&config(vec![])).unwrap();
        let editor = joined(&server);
        push_mod(&editor, &cache_mod("jei.jar", &"a".repeat(40)));
        let update = editor.state();

        server.apply(&update).unwrap();
        server.apply(&update).unwrap();
        assert_eq!(back(&server).mods.len(), 1);
    }
}
