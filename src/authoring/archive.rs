//! Instance archive extraction: pull `mods/*.jar` and the bundled `extra.zip`
//! tree out of a client instance archive. Used by bootstrap (staging) and
//! validate (cross-reference). Internal to the authoring layer.

use super::sources::sha1_hex;
use anyhow::{Context, Result};
use std::io::{Cursor, Read};
use tracing::warn;

pub(super) struct DiscoveredMod {
    pub(super) sha1: String,
    pub(super) filename: String,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct DiscoveredAsset {
    pub(super) rel_path: String,
    pub(super) bytes: Vec<u8>,
}

/// Per-entry uncompressed-size ceiling. A zip's declared `size` is attacker-
/// controlled, so we never pre-allocate the full declared size and hard-stop the
/// read here -- a zip bomb (tiny compressed, giant declared) would otherwise
/// drive an allocation that aborts the whole process.
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// Read a zip entry into memory with a hard ceiling on both the pre-allocation
/// and the bytes actually read, so a lying size header can't OOM-abort the
/// mirror. Used by every archive read (bootstrap + validate + mcmod.info).
pub(super) fn read_zip_entry(mut reader: impl Read, declared: u64, name: &str) -> Result<Vec<u8>> {
    if declared > MAX_ENTRY_BYTES {
        anyhow::bail!(
            "zip entry {name} declares {declared} bytes (over the {MAX_ENTRY_BYTES} cap)"
        );
    }
    let mut buf = Vec::with_capacity(declared.min(8 * 1024 * 1024) as usize);
    let read = (&mut reader)
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading zip entry {name}"))?;
    if read as u64 > MAX_ENTRY_BYTES {
        anyhow::bail!("zip entry {name} exceeds the {MAX_ENTRY_BYTES} byte cap");
    }
    Ok(buf)
}

pub(super) fn extract_mods(archive_bytes: &[u8]) -> Result<Vec<DiscoveredMod>> {
    let reader = Cursor::new(archive_bytes);
    let mut zip = zip::ZipArchive::new(reader).context("opening the instance archive as zip")?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("reading zip entry")?;
        if !entry.is_file() {
            continue;
        }
        let Ok(name) = entry.name().map(|n| n.into_owned()) else {
            continue;
        };
        let segments: Vec<&str> = name.split('/').collect();
        let is_mod = segments.first() == Some(&"mods")
            && name.ends_with(".jar")
            && segments.last().map(|s| !s.is_empty()).unwrap_or(false);
        if !is_mod {
            continue;
        }
        let filename = segments.last().unwrap().to_string();
        let size = entry.size();
        let bytes = read_zip_entry(&mut entry, size, &name)?;
        let sha1 = sha1_hex(&bytes);
        out.push(DiscoveredMod {
            sha1,
            filename,
            bytes,
        });
    }
    Ok(out)
}

pub(super) fn extract_extra_assets(archive_bytes: &[u8]) -> Result<Vec<DiscoveredAsset>> {
    let reader = Cursor::new(archive_bytes);
    let mut zip = zip::ZipArchive::new(reader).context("opening the instance archive as zip")?;
    let mut extra_zip_bytes = None;
    if let Ok(mut entry) = zip.by_name("extra.zip") {
        let size = entry.size();
        extra_zip_bytes = Some(read_zip_entry(&mut entry, size, "extra.zip")?);
    }
    let Some(bytes) = extra_zip_bytes else {
        return Ok(Vec::new());
    };

    let mut inner = zip::ZipArchive::new(Cursor::new(bytes)).context("opening extra.zip")?;
    let mut out = Vec::new();
    for i in 0..inner.len() {
        let mut entry = inner.by_index(i).context("reading extra.zip entry")?;
        if !entry.is_file() {
            continue;
        }
        let Ok(name) = entry.name().map(|n| n.into_owned()) else {
            continue;
        };
        if name.contains("..") || name.starts_with('/') {
            warn!(path = %name, "skipping suspicious extra.zip entry");
            continue;
        }
        let size = entry.size();
        let buf = read_zip_entry(&mut entry, size, &name)?;
        out.push(DiscoveredAsset {
            rel_path: name,
            bytes: buf,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_zip_entry_rejects_a_lying_oversize_header() {
        // A declared size over the cap is rejected before any allocation.
        let err = read_zip_entry(&b""[..], MAX_ENTRY_BYTES + 1, "bomb").unwrap_err();
        assert!(err.to_string().contains("cap"), "got {err}");
    }

    #[test]
    fn read_zip_entry_reads_a_normal_entry() {
        let data = b"hello world";
        let out = read_zip_entry(&data[..], data.len() as u64, "ok").unwrap();
        assert_eq!(out, data);
    }
}
