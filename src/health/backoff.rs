//! Persisted download-client backoff diagnostics. Storage reads only: no network I/O.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::db::Database;
use libsql::{Connection, TransactionBehavior, params};

/// Eligible clients use command admission's predicate; status counts only at the current revision.
const COUNTS: &str = "SELECT count(*),COALESCE(sum(st.disabled_until>?2),0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id LEFT JOIN download_client_status st ON st.provider_id=p.id AND st.media_type=s.media_type AND st.config_revision=p.revision WHERE p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=?1";
/// Earliest still-active cooldown in a scope; NULL when nothing will expire.
const NEXT_EXPIRY: &str = "SELECT min(st.disabled_until) FROM download_client_status st JOIN providers p ON p.id=st.provider_id AND p.revision=st.config_revision WHERE p.enabled=1 AND st.media_type=?1 AND st.disabled_until>?2";

fn media(identity: &HealthIdentity) -> Result<(&'static str, &'static str), &'static str> {
    if identity.check_key != "download_client_backoff" {
        return Err("check_failed");
    }
    match identity.scope {
        HealthScope::Tv => Ok(("tv", "sonarr")),
        HealthScope::Movies => Ok(("movies", "radarr")),
        _ => Err("check_failed"),
    }
}

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
    timestamp: i64,
) -> Result<Option<HealthIssue>, &'static str> {
    let (media, app) = media(identity)?;
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    let row = tx
        .query(COUNTS, params![media, timestamp])
        .await
        .map_err(|_| "storage_error")?
        .next()
        .await
        .map_err(|_| "storage_error")?
        .ok_or("check_failed")?;
    let total = row.get::<i64>(0).map_err(|_| "storage_error")?;
    let unavailable = row.get::<i64>(1).map_err(|_| "storage_error")?;
    drop(row);
    tx.commit().await.map_err(|_| "storage_error")?;
    if total < 0 || !(0..=total).contains(&unavailable) {
        return Err("check_failed");
    }
    let (severity, reason, message) = match unavailable {
        0 => return Ok(None),
        n if n == total => (
            HealthSeverity::Error,
            "download_clients_all_unavailable",
            "All download clients are unavailable due to failures.".to_string(),
        ),
        n => (
            HealthSeverity::Warning,
            "download_clients_unavailable",
            format!("{n} of {total} download clients are unavailable due to failures."),
        ),
    };
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity,
        reason: reason.into(),
        message,
        wiki_url: format!(
            "https://wiki.servarr.com/{app}/system#download-clients-are-unavailable-due-to-failures"
        ),
        compatibility_type: "DownloadClientStatusCheck".into(),
    }))
}

/// Rearm the expiry wake-up inside the publishing transaction so it matches the stored state.
pub(super) async fn rearm(
    c: &Connection,
    identity: &HealthIdentity,
    timestamp: i64,
) -> super::Result<()> {
    let Ok((media, _)) = media(identity) else {
        return Ok(());
    };
    c.execute(
        &format!("UPDATE health_checks SET next_expiry_at=({NEXT_EXPIRY}) WHERE scope=?1 AND check_key='download_client_backoff'"),
        params![media, timestamp],
    )
    .await?;
    Ok(())
}
