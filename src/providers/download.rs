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
            if kind.is_dir() {
                if depth + 1 >= MAX_OUTPUT_DEPTH {
                    // A directory below the cap hides files from the caller, and a partial
                    // listing would let the preflight judge incomplete evidence. Refuse it.
                    return Err(std::io::Error::other("output depth limit"));
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch() -> Scratch {
        let path = std::env::temp_dir().join(format!("hrrdarr-download-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    #[test]
    fn listing_within_the_depth_cap_returns_every_file() {
        let s = scratch();
        let deep = s.0.join("a/b/c/d/e");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("movie.mkv"), b"bytes").unwrap();
        std::fs::write(s.0.join("top.mkv"), b"bytes").unwrap();
        match list_local_output(&s.0).unwrap() {
            LocalListing::Directory(files) => assert_eq!(
                files,
                vec![
                    ("a/b/c/d/e/movie.mkv".to_owned(), 5),
                    ("top.mkv".to_owned(), 5)
                ]
            ),
            _ => panic!("expected a directory listing"),
        }
    }

    #[test]
    fn a_directory_below_the_depth_cap_is_refused_instead_of_truncated() {
        let s = scratch();
        let deep = s.0.join("a/b/c/d/e/f");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("movie.mkv"), b"bytes").unwrap();
        std::fs::write(s.0.join("top.mkv"), b"bytes").unwrap();
        // The deep file is invisible to a truncated listing, so the listing is refused rather
        // than letting the preflight judge incomplete evidence.
        let error = match list_local_output(&s.0) {
            Ok(_) => panic!("expected the listing to be refused"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "output depth limit");
    }
    #[test]
    fn depth_limit_fails_closed() {
        let root =
            std::env::temp_dir().join(format!("hrrdarr-output-depth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut current = root.clone();
        for depth in 0..MAX_OUTPUT_DEPTH {
            current.push(format!("level-{depth}"));
            std::fs::create_dir(&current).unwrap();
        }

        let result = list_local_output(&root);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(result.is_err());
    }
}
