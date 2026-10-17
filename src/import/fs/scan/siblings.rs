//! Bounded immediate-parent observations. These names never expand import authority.
use super::*;

#[derive(Debug)]
pub(crate) struct Siblings {
    source_path: String,
    source: Identity,
    parent: Identity,
    entries: Vec<Entry>,
}
impl Siblings {
    /// Immediate names, sorted, including explicit directory/symlink/special kinds.
    /// Consumers considering sibling files must select Kind::File, not follow links.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}
fn budget(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::conflict(
            "scan_cancelled",
            "Source observation was cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(limit());
    }
    Ok(())
}
impl Inventory {
    pub fn observe_siblings(
        &self,
        index: usize,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Siblings> {
        (|| {
            let start = Instant::now();
            let deadline = deadline.min(start + OBSERVATION_LIMIT);
            budget(deadline, cancel)?;
            let source = self.open_file(index)?;
            budget(deadline, cancel)?;
            let entry = self.entries.get(index).ok_or_else(invalid)?;
            let (parent, _) = self.entry_parent(entry, start)?;
            budget(deadline, cancel)?;
            let before = Identity::read(&parent)?;
            let mut entries = Vec::new();
            let mut bytes = 0usize;
            for item in Dir::read_from(&parent).map_err(io)? {
                budget(deadline, cancel)?;
                let item = item.map_err(io)?;
                let name = item.file_name().to_str().map_err(|_| invalid())?;
                if matches!(name, "." | "..") {
                    continue;
                }
                if entries.len() >= MAX_ENTRIES {
                    return Err(limit());
                }
                let sibling = observe(&parent, name, name.into())?;
                // No media filter and no recursion; special entries remain explicitly typed.
                bytes = bytes
                    .checked_add(sibling.durable_json()?.len() + 1)
                    .ok_or_else(limit)?;
                if bytes > MAX_MANIFEST_BYTES {
                    return Err(limit());
                }
                entries.push(sibling);
            }
            entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
            check(
                entries
                    .windows(2)
                    .all(|w| w[0].relative_path < w[1].relative_path),
            )?;
            check(before.unchanged(&Identity::read(&parent)?))?;
            self.verify_open_file_at(index, &source, start)?;
            let result = Siblings {
                source_path: self.source_path(index)?,
                source: entry.identity.clone(),
                parent: before,
                entries,
            };
            // Reopen the parent chain too: a held directory alone cannot prove its path stayed bound.
            self.verify_siblings(index, &result, deadline, cancel)?;
            Ok(result)
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }

    pub fn verify_siblings(
        &self,
        index: usize,
        siblings: &Siblings,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<()> {
        (|| {
            let start = Instant::now();
            let deadline = deadline.min(start + OBSERVATION_LIMIT);
            budget(deadline, cancel)?;
            let entry = self.entries.get(index).ok_or_else(invalid)?;
            check(
                self.source_path(index)? == siblings.source_path
                    && entry.identity.unchanged(&siblings.source),
            )?;
            let source = self.open_file(index)?;
            budget(deadline, cancel)?;
            let (parent, _) = self.entry_parent(entry, start)?;
            check(siblings.parent.unchanged(&Identity::read(&parent)?))?;
            self.verify_open_file_at(index, &source, start)?;
            budget(deadline, cancel)
        })()
        .map_err(|e: Error| e.at(PathRole::Source))
    }
}
