//! Measured scan preparation. These observations do not grant transfer authority.
use super::{
    Error, Result, probe,
    scan_fs::{Inventory, Kind},
};
use crate::{
    api::MediaDomain,
    search::{ReleaseTarget, downloaded::ScanPolicyEligible},
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const MAX_MEDIA_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MediaObservation {
    Measured {
        version: u8,
        sha256: String,
        runtime_ticks: Option<i64>,
        audio_stream_count: usize,
    },
    Unavailable {
        version: u8,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SpaceObservation {
    Skipped,
    Unavailable,
    Measured {
        available_bytes: u64,
        required_bytes: u64,
    },
}

pub(crate) struct Observed {
    pub media: MediaObservation,
    pub space: SpaceObservation,
    pub reasons: Vec<&'static str>,
    media_info: Option<String>,
}

fn invalid() -> Error {
    Error::conflict(
        "scan_observation_invalid",
        "Scan observation is invalid or changed",
    )
}
/// Proof identity is stored canonicalised (sorted keys), so compare structurally.
fn same_json(left: &str, right: &str) -> bool {
    match (
        serde_json::from_str::<serde_json::Value>(left),
        serde_json::from_str::<serde_json::Value>(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}
fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn budget(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::conflict(
            "scan_cancelled",
            "Source observation was cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(Error::conflict(
            "scan_observation_timeout",
            "Source observation exceeded its deadline",
        ));
    }
    Ok(())
}

/// Invoke on a bounded blocking worker. The complete inventory must still be
/// checked at publication; each remaining source is checked separately at start.
pub(crate) fn observe(
    inventory: &Inventory,
    index: usize,
    eligible: &ScanPolicyEligible,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Observed> {
    budget(deadline, cancel)?;
    let source = inventory.source_path(index)?;
    let entry = inventory.entries().get(index).ok_or_else(invalid)?;
    if entry.kind != Kind::File
        || entry.size() != eligible.proof.size
        || !same_json(
            &entry.identity_json()?,
            &eligible.proof.source_identity_json,
        )
        || entry.relative_path != eligible.proof.relative_path
        || Path::new(&source).file_name().and_then(|s| s.to_str())
            != Some(eligible.proof.basename.as_str())
    {
        return Err(invalid());
    }
    let domain = if matches!(eligible.target, ReleaseTarget::Tv { .. }) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    if eligible.import_policy.media_type != domain {
        return Err(invalid());
    }
    let held = inventory.open_file(index)?;
    let siblings = inventory.observe_siblings(index, deadline, cancel)?;
    let measured = probe::observe(inventory, index, deadline, cancel)?;
    budget(deadline, cancel)?;
    let mut reasons = eligible.media_rejections(&measured);
    if eligible
        .import_policy
        .source_ancestor_matches(Path::new(&source))
        .map_err(|_| invalid())?
    {
        reasons.push("working_folder");
    }
    if domain == MediaDomain::Movies {
        let parent = Path::new(&source).parent().ok_or_else(invalid)?;
        let paths: Vec<String> = siblings
            .entries()
            .iter()
            .filter(|e| e.kind == Kind::File)
            .map(|e| {
                parent
                    .join(&e.relative_path)
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            })
            .collect::<Result<_>>()?;
        if multipart(&source, &paths) {
            reasons.push("multipart_movie");
        }
    }
    let reserve = eligible
        .import_policy
        .minimum_bytes()
        .map_err(|_| invalid())?;
    let space = if eligible.import_policy.skip_free_space_check_when_importing {
        SpaceObservation::Skipped
    } else {
        // Both references observe the parent of the owner's library path. This
        // observation reserves no bytes and cannot replace disk-full recovery.
        let available = Path::new(&eligible.root)
            .parent()
            .and_then(|path| super::filesystem_directory(path).ok())
            .and_then(|fd| rustix::fs::fstatvfs(&fd).ok())
            .and_then(|stat| stat.f_bavail.checked_mul(stat.f_frsize));
        space_observation(entry.size(), reserve, available)?
    };
    if matches!(space, SpaceObservation::Measured { available_bytes, required_bytes } if available_bytes < required_bytes)
    {
        reasons.push("minimum_free_space");
    }
    let (media, media_info) = match measured {
        probe::Observation::Measured(media) => {
            let info = media
                .normalized_info(crate::import_policy::domain(domain))
                .map_err(|_| invalid())?;
            (
                MediaObservation::Measured {
                    version: 1,
                    sha256: digest(info.as_bytes()),
                    runtime_ticks: media.runtime_ticks,
                    audio_stream_count: media.audio_stream_count,
                },
                Some(info),
            )
        }
        probe::Observation::Unavailable(reason) => (
            MediaObservation::Unavailable {
                version: 1,
                reason: reason.into(),
            },
            None,
        ),
    };
    inventory.verify_siblings(index, &siblings, deadline, cancel)?;
    inventory.verify_open_file(index, &held)?;
    budget(deadline, cancel)?;
    Ok(Observed {
        media,
        space,
        reasons,
        media_info,
    })
}

fn space_observation(size: u64, reserve: i64, available: Option<u64>) -> Result<SpaceObservation> {
    let reserve: u64 = reserve.try_into().map_err(|_| invalid())?;
    let required_bytes = size.checked_add(reserve).ok_or_else(invalid)?;
    Ok(match available {
        Some(available_bytes) => SpaceObservation::Measured {
            available_bytes,
            required_bytes,
        },
        None => SpaceObservation::Unavailable,
    })
}

// Numeric and alphabetic part numbering are independent families. Compare the
// complete remaining names exactly; a lone title containing "Part 1" is valid.
fn multipart_key(path: &str, numeric: bool) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len());
    let mut cursor = 0;
    let mut kept = 0;
    while cursor < bytes.len() {
        let mut marker = cursor;
        while marker < bytes.len() && b" _.-".contains(&bytes[marker]) {
            marker += 1;
        }
        let word = ["part", "disc", "disk", "dvd", "cd", "pt", "p"]
            .iter()
            .find(|word| {
                bytes
                    .get(marker..marker + word.len())
                    .is_some_and(|s| s.eq_ignore_ascii_case(word.as_bytes()))
            });
        if cursor > 0
            && let Some(word) = word
        {
            let mut number = marker + word.len();
            while number < bytes.len() && b" _.-".contains(&bytes[number]) {
                number += 1;
            }
            let mut end = number;
            while end < bytes.len()
                && if numeric {
                    bytes[end].is_ascii_digit()
                } else {
                    (b'a'..=b'd').contains(&bytes[end].to_ascii_lowercase())
                }
            {
                end += 1;
            }
            if end > number {
                result.push_str(&path[kept..cursor]);
                cursor = end;
                kept = end;
                continue;
            }
        }
        cursor += 1;
    }
    result.push_str(&path[kept..]);
    result
}
fn multipart(source: &str, siblings: &[String]) -> bool {
    [true, false].into_iter().any(|numeric| {
        let key = multipart_key(source, numeric);
        key != source
            && siblings
                .iter()
                .filter(|name| multipart_key(name, numeric) == key)
                .take(2)
                .count()
                > 1
    })
}

impl Observed {
    /// Child and its observation descriptor must already exist in the same
    /// publication transaction. SQL checks scope, lifecycle and aggregate capacity.
    pub(crate) async fn persist_media(
        &self,
        c: &Connection,
        child: uuid::Uuid,
        command: uuid::Uuid,
    ) -> Result<()> {
        if c.is_autocommit() {
            return Err(invalid());
        }
        if let (
            MediaObservation::Measured {
                version: 1, sha256, ..
            },
            Some(info),
        ) = (&self.media, &self.media_info)
        {
            if info.len() > MAX_MEDIA_BYTES || digest(info.as_bytes()) != *sha256 {
                return Err(invalid());
            }
            c.execute("INSERT INTO downloaded_scan_media_observations(child_id,command_id,media_info_json,sha256) VALUES(?1,?2,?3,?4)", params![child.to_string(),command.to_string(),info.clone(),sha256.clone()]).await?;
        } else if !matches!(
            (&self.media, &self.media_info),
            (MediaObservation::Unavailable { version: 1, .. }, None)
        ) {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Read retained metadata without probing moved source bytes. The immutable
/// descriptor supplies the expected digest; a sidecar never supplies authority.
pub(crate) async fn retained_media(
    c: &Connection,
    child: uuid::Uuid,
    command: uuid::Uuid,
    expected: &MediaObservation,
) -> Result<Option<String>> {
    let mut rows = c.query("SELECT substr(CAST(o.media_info_json AS BLOB),1,65537),o.sha256,c.media_type FROM downloaded_scan_media_observations o JOIN downloaded_scan_commands c ON c.id=o.command_id WHERE o.child_id=?1 AND o.command_id=?2", params![child.to_string(),command.to_string()]).await?;
    let row = rows.next().await?;
    match (expected, row) {
        (MediaObservation::Unavailable { version: 1, .. }, None) => Ok(None),
        (
            MediaObservation::Measured {
                version: 1,
                sha256,
                runtime_ticks,
                audio_stream_count,
            },
            Some(row),
        ) => {
            let bytes: Vec<u8> = row.get(0)?;
            if bytes.len() > MAX_MEDIA_BYTES
                || digest(&bytes) != *sha256
                || row.get::<String>(1)? != *sha256
                || runtime_ticks.is_some_and(|v| v < 0)
                || *audio_stream_count > 64
            {
                return Err(invalid());
            }
            let raw = String::from_utf8(bytes).map_err(|_| invalid())?;
            let info: crate::media_files::media_info::MediaInfo =
                serde_json::from_str(&raw).map_err(|_| invalid())?;
            if serde_json::to_string(&info).map_err(|_| invalid())? != raw {
                return Err(invalid());
            }
            let fields = serde_json::to_value(&info).map_err(|_| invalid())?;
            let count = match row.get::<String>(2)?.as_str() {
                "tv" => fields["audio_streams"]
                    .as_array()
                    .map(|streams| streams.len()),
                "movies" => fields["audio_stream_count"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok()),
                _ => return Err(invalid()),
            };
            if fields["runtime_ticks"].as_i64() != *runtime_ticks
                || count != Some(*audio_stream_count)
            {
                return Err(invalid());
            }
            Ok(Some(raw))
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multipart_families_preserve_legitimate_titles_and_space_boundaries() {
        for (a, b) in [
            ("part1", "part2"),
            ("pt1", "pt2"),
            ("P1", "P2"),
            ("cd 1", "cd 2"),
            ("dvd a", "dvd b"),
        ] {
            let files = vec![
                format!("/downloads/Film {a}.mkv"),
                format!("/downloads/Film {b}.mkv"),
            ];
            assert!(multipart(&files[0], &files));
        }
        for files in [
            vec!["/downloads/Deathly Hallows Part 1.mkv"],
            vec!["/downloads/Film cd a.mkv", "/downloads/Film cd 2.mkv"],
            vec!["/downloads/Film cd a.mkv", "/downloads/Other cd b.mkv"],
        ] {
            let files = files.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            assert!(!multipart(&files[0], &files));
        }
        let required = 100 * 1024 * 1024 + 123;
        assert!(
            matches!(space_observation(123,100*1024*1024,Some(required)).unwrap(),SpaceObservation::Measured{available_bytes,required_bytes} if available_bytes==required_bytes)
        );
        assert!(
            matches!(space_observation(123,100*1024*1024,Some(required-1)).unwrap(),SpaceObservation::Measured{available_bytes,required_bytes} if available_bytes<required_bytes)
        );
        assert_eq!(
            space_observation(123, 100, None).unwrap(),
            SpaceObservation::Unavailable
        );
        assert!(space_observation(u64::MAX, 1, None).is_err());
    }
}
