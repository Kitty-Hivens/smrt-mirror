//! Repackage (tamper) diff: compare a self-hosted jar against its genuine
//! Modrinth counterpart and report which entries differ, class files kept
//! separate from resource churn. A repackaged jar typically differs from the
//! genuine build in a large number of reformatted resources but only a handful
//! of classes -- and it is the changed classes an operator cares about, because
//! a behaviour patch hides in the resource noise. Read-only; no action.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Cursor;
use ts_rs::TS;

/// The entries that differ between a repackaged jar and its genuine counterpart.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct JarDiff {
    /// Same path, different bytes, `.class` -- the tamper that matters.
    pub changed_classes: Vec<String>,
    /// Same path, different bytes, not a class -- usually reformatting noise.
    pub changed_resources: Vec<String>,
    /// Present only in the repackaged jar.
    pub added: Vec<String>,
    /// Present only in the genuine jar.
    pub removed: Vec<String>,
    /// Entries byte-identical in both (context for how much matched).
    #[ts(type = "number")]
    pub identical: usize,
}

/// Index a jar's file entries to their CRC-32. The zip central directory carries
/// the CRC, so this reads no compressed data. Directories are skipped.
fn crc_index(bytes: &[u8]) -> Result<HashMap<String, u32>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("opening jar as zip")?;
    let mut out = HashMap::with_capacity(zip.len());
    for i in 0..zip.len() {
        let entry = zip.by_index(i).context("reading zip entry")?;
        if entry.is_dir() {
            continue;
        }
        let Ok(name) = entry.name().map(|n| n.into_owned()) else {
            continue;
        };
        out.insert(name, entry.crc32());
    }
    Ok(out)
}

/// Compare `repack` against the `genuine` jar by entry CRC. Pure.
pub fn diff_jars(repack: &[u8], genuine: &[u8]) -> Result<JarDiff> {
    let a = crc_index(repack)?;
    let b = crc_index(genuine)?;

    let mut changed_classes = Vec::new();
    let mut changed_resources = Vec::new();
    let mut added = Vec::new();
    let mut identical = 0usize;
    for (name, crc) in &a {
        match b.get(name) {
            Some(other) if other == crc => identical += 1,
            Some(_) if name.ends_with(".class") => changed_classes.push(name.clone()),
            Some(_) => changed_resources.push(name.clone()),
            None => added.push(name.clone()),
        }
    }
    let mut removed: Vec<String> = b.keys().filter(|n| !a.contains_key(*n)).cloned().collect();

    changed_classes.sort();
    changed_resources.sort();
    added.sort();
    removed.sort();
    Ok(JarDiff {
        changed_classes,
        changed_resources,
        added,
        removed,
        identical,
    })
}

#[cfg(test)]
mod tests {
    use super::diff_jars;
    use crate::authoring::classfile::fixtures::jar;

    #[test]
    fn separates_class_changes_from_resource_churn() {
        let genuine = jar(&[
            ("a/Foo.class", b"CLASSBYTES1"),
            ("assets/lang.json", b"{}"),
            ("META-INF/keep", b"same"),
            ("gone.txt", b"dropped"),
        ]);
        let repack = jar(&[
            ("a/Foo.class", b"CLASSBYTES2"), // changed class -- the tamper
            ("assets/lang.json", b"{ }"),    // changed resource -- reformat noise
            ("META-INF/keep", b"same"),      // byte-identical
            ("extra.txt", b"added"),         // only in repack
        ]);
        let d = diff_jars(&repack, &genuine).unwrap();
        assert_eq!(d.changed_classes, ["a/Foo.class"]);
        assert_eq!(d.changed_resources, ["assets/lang.json"]);
        assert_eq!(d.added, ["extra.txt"]);
        assert_eq!(d.removed, ["gone.txt"]);
        assert_eq!(d.identical, 1);
    }

    #[test]
    fn identical_jars_report_no_changes() {
        let j = jar(&[("a.class", b"x"), ("b.txt", b"y")]);
        let d = diff_jars(&j, &j).unwrap();
        assert!(d.changed_classes.is_empty());
        assert!(d.changed_resources.is_empty());
        assert!(d.added.is_empty());
        assert!(d.removed.is_empty());
        assert_eq!(d.identical, 2);
    }
}
