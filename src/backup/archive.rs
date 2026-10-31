//! Minimal stored (uncompressed) ZIP container for hrrdarr backups. Standard tools can list and
//! extract it, but the reader only accepts the strict layout this writer produces: named entries
//! from a fixed allow-list, contiguous local records, then the central directory and an end record
//! without comment. No compression, encryption, ZIP64, data descriptors, extra fields or
//! archive-supplied paths: entry names are matched against constants and never joined to a path.
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Without ZIP64 every offset and size must fit in 32 bits.
pub(crate) const MAX_ARCHIVE_BYTES: u64 = u32::MAX as u64;
const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const LOCAL_LEN: u64 = 30;
const CENTRAL_LEN: usize = 46;
const END_LEN: u64 = 22;
const NEEDED: u16 = 20;
const MADE_BY_UNIX: u16 = (3 << 8) | 20;
const MAX_ENTRIES: usize = 8;
const MAX_NAME: usize = 64;
const CHUNK: usize = 64 * 1024;

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// CRC-32 (IEEE 802.3), as required by the ZIP entry records.
#[derive(Clone)]
pub(crate) struct Crc32(u32);
impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for byte in bytes {
            c = CRC_TABLE[((c ^ u32::from(*byte)) & 0xff) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }
    pub(crate) fn finish(&self) -> u32 {
        !self.0
    }
}

#[derive(Debug)]
pub(crate) enum ArchiveError {
    Invalid(&'static str),
    Io(io::Error),
}
impl From<io::Error> for ArchiveError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Self::Invalid("truncated archive")
        } else {
            Self::Io(error)
        }
    }
}

/// DOS date/time (UTC) for the entry records; clamped to the format's 1980 epoch.
fn dos_time(unix: i64) -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let Some(t) = chrono::DateTime::from_timestamp(unix.max(315_532_800), 0) else {
        return (0, 0x21);
    };
    let year = (t.year() - 1980).clamp(0, 127) as u16;
    let time = ((t.hour() as u16) << 11) | ((t.minute() as u16) << 5) | (t.second() as u16 / 2);
    let date = (year << 9) | ((t.month() as u16) << 5) | t.day() as u16;
    (time, date)
}

struct Written {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

/// Streams entries whose size and CRC are known before their bytes are written.
pub(crate) struct Writer<W: Write> {
    out: W,
    offset: u64,
    limit: u64,
    entries: Vec<Written>,
    time: (u16, u16),
}
impl<W: Write> Writer<W> {
    pub(crate) fn new(out: W, created_at: i64, limit: u64) -> Self {
        Self {
            out,
            offset: 0,
            limit: limit.min(MAX_ARCHIVE_BYTES),
            entries: Vec::new(),
            time: dos_time(created_at),
        }
    }
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        let next = self.offset + bytes.len() as u64;
        if next > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "backup archive size limit reached",
            ));
        }
        self.out.write_all(bytes)?;
        self.offset = next;
        Ok(())
    }
    /// Copies exactly `size` bytes from `source`; `observe` sees every chunk (and may abort).
    pub(crate) fn entry(
        &mut self,
        name: &str,
        size: u64,
        crc: u32,
        source: &mut dyn Read,
        observe: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if name.is_empty() || name.len() > MAX_NAME || self.entries.len() >= MAX_ENTRIES {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "entry"));
        }
        let size32 = u32::try_from(size)
            .map_err(|_| io::Error::new(io::ErrorKind::FileTooLarge, "entry too large"))?;
        let offset = u32::try_from(self.offset)
            .map_err(|_| io::Error::new(io::ErrorKind::FileTooLarge, "archive too large"))?;
        let mut header = Vec::with_capacity(LOCAL_LEN as usize + name.len());
        header.extend_from_slice(&LOCAL.to_le_bytes());
        header.extend_from_slice(&NEEDED.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // flags
        header.extend_from_slice(&0u16.to_le_bytes()); // stored
        header.extend_from_slice(&self.time.0.to_le_bytes());
        header.extend_from_slice(&self.time.1.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&size32.to_le_bytes());
        header.extend_from_slice(&size32.to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra
        header.extend_from_slice(name.as_bytes());
        self.put(&header)?;
        let mut remaining = size;
        let mut buffer = vec![0u8; CHUNK];
        let mut check = Crc32::new();
        while remaining > 0 {
            let want = remaining.min(CHUNK as u64) as usize;
            source.read_exact(&mut buffer[..want])?;
            observe(&buffer[..want])?;
            check.update(&buffer[..want]);
            self.put(&buffer[..want])?;
            remaining -= want as u64;
        }
        if check.finish() != crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "entry changed while archiving",
            ));
        }
        self.entries.push(Written {
            name: name.to_owned(),
            crc,
            size: size32,
            offset,
        });
        Ok(())
    }
    /// Writes the central directory and end record; returns the sink and total size.
    pub(crate) fn finish(mut self) -> io::Result<(W, u64)> {
        let start = u32::try_from(self.offset)
            .map_err(|_| io::Error::new(io::ErrorKind::FileTooLarge, "archive too large"))?;
        let mut directory = Vec::new();
        for e in &self.entries {
            directory.extend_from_slice(&CENTRAL.to_le_bytes());
            directory.extend_from_slice(&MADE_BY_UNIX.to_le_bytes());
            directory.extend_from_slice(&NEEDED.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes());
            directory.extend_from_slice(&self.time.0.to_le_bytes());
            directory.extend_from_slice(&self.time.1.to_le_bytes());
            directory.extend_from_slice(&e.crc.to_le_bytes());
            directory.extend_from_slice(&e.size.to_le_bytes());
            directory.extend_from_slice(&e.size.to_le_bytes());
            directory.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&0u16.to_le_bytes()); // extra
            directory.extend_from_slice(&0u16.to_le_bytes()); // comment
            directory.extend_from_slice(&0u16.to_le_bytes()); // disk
            directory.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            directory.extend_from_slice(&(0o100600u32 << 16).to_le_bytes());
            directory.extend_from_slice(&e.offset.to_le_bytes());
            directory.extend_from_slice(e.name.as_bytes());
        }
        let count = self.entries.len() as u16;
        let mut end = Vec::with_capacity(END_LEN as usize);
        end.extend_from_slice(&END.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        end.extend_from_slice(&start.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.put(&directory)?;
        self.put(&end)?;
        self.out.flush()?;
        Ok((self.out, self.offset))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: &'static str,
    pub data_offset: u64,
    pub size: u64,
    pub crc: u32,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Parses and cross-checks the archive structure. Every name must appear exactly once.
pub(crate) fn entries<R: Read + Seek>(
    file: &mut R,
    len: u64,
    names: &[&'static str],
) -> Result<Vec<Entry>, ArchiveError> {
    let invalid = ArchiveError::Invalid;
    if !(END_LEN..=MAX_ARCHIVE_BYTES).contains(&len) {
        return Err(invalid("archive size outside supported range"));
    }
    let mut end = [0u8; END_LEN as usize];
    file.seek(SeekFrom::Start(len - END_LEN))?;
    file.read_exact(&mut end)?;
    if u32_at(&end, 0) != END
        || u16_at(&end, 4) != 0
        || u16_at(&end, 6) != 0
        || u16_at(&end, 8) != u16_at(&end, 10)
        || u16_at(&end, 20) != 0
    {
        return Err(invalid("missing or unsupported end record"));
    }
    let count = usize::from(u16_at(&end, 10));
    let directory_len = u64::from(u32_at(&end, 12));
    let directory_at = u64::from(u32_at(&end, 16));
    if count != names.len()
        || directory_len > (CENTRAL_LEN + MAX_NAME) as u64 * MAX_ENTRIES as u64
        || directory_at.checked_add(directory_len) != Some(len - END_LEN)
    {
        return Err(invalid("unexpected archive directory"));
    }
    let mut directory = vec![0u8; directory_len as usize];
    file.seek(SeekFrom::Start(directory_at))?;
    file.read_exact(&mut directory)?;
    let mut found: Vec<(Entry, u64)> = Vec::with_capacity(count);
    let mut at = 0usize;
    for _ in 0..count {
        if directory.len() < at + CENTRAL_LEN {
            return Err(invalid("truncated directory record"));
        }
        let r = &directory[at..];
        let name_len = usize::from(u16_at(r, 28));
        if u32_at(r, 0) != CENTRAL
            || u16_at(r, 6) > NEEDED
            || u16_at(r, 8) != 0
            || u16_at(r, 10) != 0
            || u32_at(r, 20) != u32_at(r, 24)
            || u16_at(r, 30) != 0
            || u16_at(r, 32) != 0
            || u16_at(r, 34) != 0
            || name_len == 0
            || name_len > MAX_NAME
            || directory.len() < at + CENTRAL_LEN + name_len
        {
            return Err(invalid("unsupported directory record"));
        }
        let raw = &r[CENTRAL_LEN..CENTRAL_LEN + name_len];
        let Some(name) = names.iter().copied().find(|n| n.as_bytes() == raw) else {
            return Err(invalid("unexpected archive entry"));
        };
        if found.iter().any(|(e, _)| e.name == name) {
            return Err(invalid("duplicate archive entry"));
        }
        let local = u64::from(u32_at(r, 42));
        found.push((
            Entry {
                name,
                data_offset: local + LOCAL_LEN + name_len as u64,
                size: u64::from(u32_at(r, 24)),
                crc: u32_at(r, 16),
            },
            local,
        ));
        at += CENTRAL_LEN + name_len;
    }
    if at != directory.len() {
        return Err(invalid("trailing directory bytes"));
    }
    found.sort_by_key(|(_, local)| *local);
    let mut expected = 0u64;
    for (entry, local) in &found {
        if *local != expected {
            return Err(invalid("non-contiguous archive layout"));
        }
        let mut header = [0u8; LOCAL_LEN as usize + MAX_NAME];
        let name_len = entry.name.len();
        file.seek(SeekFrom::Start(*local))?;
        file.read_exact(&mut header[..LOCAL_LEN as usize + name_len])?;
        if u32_at(&header, 0) != LOCAL
            || u16_at(&header, 4) > NEEDED
            || u16_at(&header, 6) != 0
            || u16_at(&header, 8) != 0
            || u32_at(&header, 14) != entry.crc
            || u64::from(u32_at(&header, 18)) != entry.size
            || u64::from(u32_at(&header, 22)) != entry.size
            || usize::from(u16_at(&header, 26)) != name_len
            || u16_at(&header, 28) != 0
            || &header[LOCAL_LEN as usize..LOCAL_LEN as usize + name_len] != entry.name.as_bytes()
        {
            return Err(invalid("local record does not match directory"));
        }
        expected = entry
            .data_offset
            .checked_add(entry.size)
            .ok_or(invalid("entry overflow"))?;
    }
    if expected != directory_at {
        return Err(invalid("unexpected bytes before directory"));
    }
    Ok(found.into_iter().map(|(e, _)| e).collect())
}

/// Streams one entry's bytes to `observe`, verifying its CRC at the end.
pub(crate) fn read_entry<R: Read + Seek>(
    file: &mut R,
    entry: &Entry,
    observe: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    file.seek(SeekFrom::Start(entry.data_offset))?;
    let mut remaining = entry.size;
    let mut buffer = vec![0u8; CHUNK];
    let mut crc = Crc32::new();
    while remaining > 0 {
        let want = remaining.min(CHUNK as u64) as usize;
        file.read_exact(&mut buffer[..want])?;
        crc.update(&buffer[..want]);
        observe(&buffer[..want])?;
        remaining -= want as u64;
    }
    if crc.finish() != entry.crc {
        return Err(ArchiveError::Invalid("entry checksum mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn build(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = Writer::new(Vec::new(), 1_700_000_000, MAX_ARCHIVE_BYTES);
        for (name, bytes) in entries {
            let mut crc = Crc32::new();
            crc.update(bytes);
            writer
                .entry(
                    name,
                    bytes.len() as u64,
                    crc.finish(),
                    &mut Cursor::new(bytes),
                    &mut |_| Ok(()),
                )
                .unwrap();
        }
        writer.finish().unwrap().0
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn written_archives_round_trip_and_reject_tampering() {
        let bytes = build(&[("manifest.json", b"{}"), ("database.db", b"payload")]);
        let names = ["manifest.json", "database.db"];
        let found = entries(&mut Cursor::new(&bytes), bytes.len() as u64, &names).unwrap();
        assert_eq!(found.len(), 2);
        let mut out = Vec::new();
        read_entry(&mut Cursor::new(&bytes), &found[1], &mut |c| {
            out.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
        assert_eq!(out, b"payload");
        // A flipped payload byte is caught by the entry CRC, not silently accepted.
        let mut flipped = bytes.clone();
        let at = found[1].data_offset as usize;
        flipped[at] ^= 1;
        assert!(matches!(
            read_entry(&mut Cursor::new(&flipped), &found[1], &mut |_| Ok(())),
            Err(ArchiveError::Invalid(_))
        ));
        // Trailing data, unknown names, missing names and traversal names are refused.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(entries(&mut Cursor::new(&trailing), trailing.len() as u64, &names).is_err());
        let evil = build(&[("manifest.json", b"{}"), ("../database.db", b"x")]);
        assert!(entries(&mut Cursor::new(&evil), evil.len() as u64, &names).is_err());
        let single = build(&[("manifest.json", b"{}")]);
        assert!(entries(&mut Cursor::new(&single), single.len() as u64, &names).is_err());
        let dup = build(&[("manifest.json", b"{}"), ("manifest.json", b"{}")]);
        assert!(entries(&mut Cursor::new(&dup), dup.len() as u64, &names).is_err());
        assert!(entries(&mut Cursor::new(b"not a zip"), 9, &names).is_err());
    }

    #[test]
    fn writer_enforces_the_size_limit_mid_entry() {
        let mut writer = Writer::new(Vec::new(), 0, 100);
        let payload = vec![7u8; 1000];
        let mut crc = Crc32::new();
        crc.update(&payload);
        let error = writer
            .entry(
                "database.db",
                1000,
                crc.finish(),
                &mut Cursor::new(&payload),
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
    }
}
