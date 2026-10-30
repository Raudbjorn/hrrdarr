//! Versioned, bounded restart evidence. Restoration never invents new file identities.
use super::*;
use std::collections::BTreeMap;

const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
const MAX_ENTRY_BYTES: usize = 32 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    media: MediaDomain,
    source_path: String,
    ancestor_count: usize,
    ancestor_digest: String,
    root: Entry,
    entry_count: usize,
    entries_digest: String,
}

// Identity's ordinary journal decoder is intentionally not changed by this format.
pub(super) fn identity<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Identity, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        dev: u64,
        ino: u64,
        size: u64,
        mtime: i64,
        mtime_ns: i64,
        ctime: i64,
        ctime_ns: i64,
    }
    let v = Strict::deserialize(d)?;
    if !(0..1_000_000_000).contains(&v.mtime_ns) || !(0..1_000_000_000).contains(&v.ctime_ns) {
        return Err(serde::de::Error::custom("invalid identity timestamp"));
    }
    Ok(Identity {
        dev: v.dev,
        ino: v.ino,
        size: v.size,
        mtime: v.mtime,
        mtime_ns: v.mtime_ns,
        ctime: v.ctime,
        ctime_ns: v.ctime_ns,
    })
}

fn hex(context: Context) -> String {
    let mut result = String::with_capacity(64);
    for byte in context.finish().as_ref() {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}
fn ancestor_digest(ancestors: &[Identity]) -> String {
    let mut hash = Context::new(&SHA256);
    hash.update(b"hrrdarr-scan-ancestors-v1\0");
    hash.update(&(ancestors.len() as u64).to_be_bytes());
    for identity in ancestors {
        hash.update(&identity.dev.to_be_bytes());
        hash.update(&identity.ino.to_be_bytes());
    }
    hex(hash)
}
fn entry_digest(entries: &[Entry]) -> Result<String> {
    let mut hash = Context::new(&SHA256);
    hash.update(b"hrrdarr-scan-entries-v1\0");
    for entry in entries {
        let encoded = entry.durable_json()?;
        hash.update(&(encoded.len() as u64).to_be_bytes());
        hash.update(encoded.as_bytes());
    }
    Ok(hex(hash))
}
fn valid_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\0')
        || path
            .split('/')
            .any(|c| c.is_empty() || matches!(c, "." | ".."))
        || path.split('/').count() > MAX_DEPTH
    {
        return Err(invalid());
    }
    Ok(())
}
impl Entry {
    pub fn durable_json(&self) -> Result<String> {
        let value = serde_json::to_string(self).map_err(|_| invalid())?;
        if value.len() > MAX_ENTRY_BYTES {
            return Err(limit());
        }
        Ok(value)
    }
}
impl Inventory {
    pub fn source_path(&self, index: usize) -> Result<String> {
        let entry = self.entries.get(index).ok_or_else(invalid)?;
        Ok(if self.root.kind == Kind::File {
            self.source_path.clone()
        } else {
            format!("{}/{}", self.source_path, entry.relative_path)
        })
    }

    pub fn envelope_json(&self, media: MediaDomain) -> Result<String> {
        let value = serde_json::to_string(&Envelope {
            version: 1,
            media,
            source_path: self.source_path.clone(),
            ancestor_count: self.ancestors.len(),
            ancestor_digest: ancestor_digest(&self.ancestors),
            root: self.root.clone(),
            entry_count: self.entries.len(),
            entries_digest: entry_digest(&self.entries)?,
        })
        .map_err(|_| invalid())?;
        let bytes = self.entries.iter().try_fold(value.len(), |n, e| {
            n.checked_add(e.durable_json()?.len() + 1).ok_or_else(limit)
        })?;
        if value.len() > MAX_ENVELOPE_BYTES || bytes > MAX_MANIFEST_BYTES {
            return Err(limit());
        }
        Ok(value)
    }

    /// Read-only restore. Callers must supply the command's independently scoped domain/path.
    /// Missing completed siblings are allowed; open_file verifies the selected historical source.
    pub fn restore(
        media: MediaDomain,
        source_path: &str,
        envelope: &[u8],
        rows: &[&[u8]],
    ) -> Result<Self> {
        (|| {
            let start = Instant::now();
            validate_path(source_path)?;
            if envelope.len() > MAX_ENVELOPE_BYTES || rows.len() > MAX_ENTRIES {
                return Err(limit());
            }
            // Check raw sizes before deserialization/allocation, including escaped strings.
            let mut bytes = envelope.len();
            for row in rows {
                if row.len() > MAX_ENTRY_BYTES {
                    return Err(limit());
                }
                bytes = bytes.checked_add(row.len() + 1).ok_or_else(limit)?;
                if bytes > MAX_MANIFEST_BYTES {
                    return Err(limit());
                }
            }
            let saved: Envelope = serde_json::from_slice(envelope).map_err(|_| invalid())?;
            if saved.version != 1
                || saved.media != media
                || saved.source_path != source_path
                || saved.entry_count != rows.len()
                || !valid_hex(&saved.ancestor_digest)
                || !valid_hex(&saved.entries_digest)
            {
                return Err(invalid());
            }
            let (parent_path, basename) = parts(source_path)?;
            let expected_count = parent_path.components().count(); // Includes Unix root.
            if saved.ancestor_count != expected_count
                || saved.root.relative_path != basename
                || !matches!(saved.root.kind, Kind::File | Kind::Directory)
            {
                return Err(invalid());
            }
            let mut entries = Vec::with_capacity(rows.len());
            let mut parents = BTreeMap::new();
            let mut previous: Option<String> = None;
            for row in rows {
                within_time(start)?;
                let entry: Entry = serde_json::from_slice(row).map_err(|_| invalid())?;
                relative(&entry.relative_path)?;
                if previous.as_ref().is_some_and(|p| p >= &entry.relative_path) {
                    return Err(invalid());
                }
                if saved.root.kind == Kind::Directory {
                    if source_path.len() + 1 + entry.relative_path.len() > 4096 {
                        return Err(limit());
                    }
                    if let Some((parent, _)) = entry.relative_path.rsplit_once('/')
                        && parents.get(parent) != Some(&Kind::Directory)
                    {
                        return Err(invalid());
                    }
                }
                previous = Some(entry.relative_path.clone());
                parents.insert(entry.relative_path.clone(), entry.kind);
                entries.push(entry);
            }
            if saved.root.kind == Kind::File
                && (entries.len() != 1
                    || entries[0].kind != Kind::File
                    || entries[0].relative_path != basename
                    || !entries[0].identity.unchanged(&saved.root.identity))
            {
                return Err(invalid());
            }
            if entry_digest(&entries)? != saved.entries_digest {
                return Err(invalid());
            }
            let (_, ancestors) = parent_chain(source_path, None, start)?;
            check(
                ancestors.len() == saved.ancestor_count
                    && ancestor_digest(&ancestors) == saved.ancestor_digest,
            )?;
            // Only ancestor dev/ino are used by subsequent replay. Historical root and every
            // entry retain their original full identity; no live file is adopted as baseline.
            within_time(start)?;
            Ok(Self {
                source_path: source_path.into(),
                ancestors,
                root: saved.root,
                entries,
            })
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }
}
