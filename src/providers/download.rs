//! Protocol-aware download-client seam. Callers hold these closed enums and dispatch through
//! [`super::RefreshClient`]; each concrete client keeps its own wire protocol module.
//!
//! Adding a client: give it a `ProviderSettings` variant, register it in the
//! `provider_implementations` table with its kind/protocol, and add one arm to each match below
//! and in `RefreshClient`. Grab, refresh, completed-download processing and health consumers do
//! not name concrete clients.
use super::{label_client, nzbget, qbittorrent, sabnzbd, transmission};

/// What the grab pipeline hands to a client. The variant must match the client's protocol; a
/// torrent source is shared by every torrent client (qBittorrent, Transmission).
pub(crate) enum ReleaseSource {
    Torrent {
        source: qbittorrent::AddSource,
        options: qbittorrent::QbitOptions,
    },
    Usenet {
        url: String,
        title: String,
    },
}
impl ReleaseSource {
    pub(crate) fn protocol(&self) -> super::Protocol {
        match self {
            Self::Torrent { .. } => super::Protocol::Torrent,
            Self::Usenet { .. } => super::Protocol::Usenet,
        }
    }
}
/// A prepared, not yet dispatched, submission. Its identity must be journaled before submit.
pub(crate) enum PreparedSubmission {
    Torrent(qbittorrent::PreparedDownload),
    Usenet(sabnzbd::PreparedNzb),
    Nzbget(nzbget::PreparedNzb),
    Transmission(transmission::PreparedTorrent),
    Label(label_client::PreparedTorrent),
}
impl PreparedSubmission {
    pub(crate) fn identity(&self) -> &qbittorrent::SubmissionIdentity {
        match self {
            Self::Torrent(prepared) => prepared.identity(),
            Self::Usenet(prepared) => prepared.identity(),
            Self::Nzbget(prepared) => prepared.identity(),
            Self::Transmission(prepared) => prepared.identity(),
            Self::Label(prepared) => prepared.identity(),
        }
    }
    pub(crate) fn into_identity(self) -> qbittorrent::SubmissionIdentity {
        match self {
            Self::Torrent(prepared) => prepared.into_identity(),
            Self::Usenet(prepared) => prepared.into_identity(),
            Self::Nzbget(prepared) => prepared.into_identity(),
            Self::Transmission(prepared) => prepared.into_identity(),
            Self::Label(prepared) => prepared.into_identity(),
        }
    }
}
/// Protocol-neutral completed-download facts consumed by owned processing. Paths are remote.
pub(crate) struct DownloadDetails {
    pub item: qbittorrent::DownloadItem,
    pub save_path: String,
    pub files: Vec<qbittorrent::TorrentFile>,
}
impl From<qbittorrent::TorrentDetails> for DownloadDetails {
    fn from(value: qbittorrent::TorrentDetails) -> Self {
        Self {
            item: value.item,
            save_path: value.save_path,
            files: value.files,
        }
    }
}
impl From<transmission::TransmissionDetails> for DownloadDetails {
    fn from(value: transmission::TransmissionDetails) -> Self {
        Self {
            item: value.item,
            save_path: value.save_path,
            files: value.files,
        }
    }
}

const MAX_OUTPUT_FILES: usize = 2048;
const MAX_OUTPUT_DEPTH: usize = 6;
pub(crate) enum LocalListing {
    /// Relative `/`-separated names and sizes beneath a directory output.
    Directory(Vec<(String, u64)>),
    /// The output itself is a single regular file.
    File(u64),
    Missing,
}
/// Bounded enumeration of a locally mapped completed output. Symlinks and special files are
/// skipped; nothing is opened for reading. Must run on a blocking thread.
pub(crate) fn list_local_output(root: &std::path::Path) -> std::io::Result<LocalListing> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalListing::Missing);
        }
        Err(error) => return Err(error),
    };
    if metadata.is_file() {
        return Ok(LocalListing::File(metadata.len()));
    }
    if !metadata.is_dir() {
        return Ok(LocalListing::Missing);
    }
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((directory, prefix, depth)) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() && depth + 1 < MAX_OUTPUT_DEPTH {
                pending.push((entry.path(), relative, depth + 1));
            } else if kind.is_file() {
                if files.len() == MAX_OUTPUT_FILES {
                    return Err(std::io::Error::other("output file limit"));
                }
                files.push((relative, entry.metadata()?.len()));
            }
        }
    }
    files.sort();
    Ok(LocalListing::Directory(files))
}
