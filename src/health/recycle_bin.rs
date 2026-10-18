//! hc.036 RecyclingBinCheck: a configured recycle bin must exist and accept a private probe file.
//! An unset bin is healthy. Writability does not prove a later transfer or retention succeeds.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::{api::MediaDomain, db::Database};

/// Keeps the message inside the stored 4096-byte bound with a long configured path.
const PATH_DISPLAY_BYTES: usize = 1024;

fn display(path: &str) -> String {
    let mut shown = String::new();
    for c in path.chars() {
        let c = if c.is_control() { '?' } else { c };
        if shown.len() + c.len_utf8() > PATH_DISPLAY_BYTES {
            shown.push('…');
            break;
        }
        shown.push(c);
    }
    shown
}

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>, &'static str> {
    let (media, wiki) = match identity.scope {
        HealthScope::Tv => (
            MediaDomain::Tv,
            "https://wiki.servarr.com/sonarr/system#cannot-write-recycle-bin",
        ),
        HealthScope::Movies => (
            MediaDomain::Movies,
            "https://wiki.servarr.com/radarr/system#cannot-write-recycle-bin",
        ),
        _ => return Err("check_failed"),
    };
    if identity.check_key != "recycle_bin" {
        return Err("check_failed");
    }
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let settings = crate::recycle_bin::read(&c, media)
        .await
        .map_err(|_| "storage_error")?;
    drop(c);
    let Some(bin) = settings.path else {
        return Ok(None);
    };
    let probe = bin.clone();
    let outcome = tokio::task::spawn_blocking(move || crate::recycle_bin::fsops::writable(&probe))
        .await
        .map_err(|_| "check_failed")?;
    let (reason, message) = match outcome {
        Ok(()) => return Ok(None),
        Err(crate::recycle_bin::fsops::FsError::Missing) => (
            "recycle_bin_missing",
            format!(
                "The configured recycling bin folder does not exist: {}. File deletion is refused until it exists.",
                display(&bin)
            ),
        ),
        Err(crate::recycle_bin::fsops::FsError::Unsafe) => (
            "recycle_bin_unwritable",
            format!(
                "Unable to write to the configured recycling bin folder {}: the path traverses a symlink or is not a folder.",
                display(&bin)
            ),
        ),
        Err(_) => (
            "recycle_bin_unwritable",
            format!(
                "Unable to write to the configured recycling bin folder: {}. Ensure this path exists and is writable by the user running hrrdarr.",
                display(&bin)
            ),
        ),
    };
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity: HealthSeverity::Error,
        reason: reason.into(),
        message,
        wiki_url: wiki.into(),
        compatibility_type: "RecyclingBinCheck".into(),
    }))
}
