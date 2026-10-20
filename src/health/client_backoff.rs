//! Current polling suppression, evaluated without probing a client.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use libsql::{Connection, params};

pub(super) async fn evaluate(
    c: &Connection,
    identity: &HealthIdentity,
    timestamp: i64,
) -> Result<Option<HealthIssue>, &'static str> {
    let (media, app) = match identity.scope {
        HealthScope::Tv => ("tv", "sonarr"),
        HealthScope::Movies => ("movies", "radarr"),
        _ => return Err("check_failed"),
    };
    let row = c.query("SELECT count(*),COALESCE(sum(CASE WHEN b.disabled_until>?2 THEN 1 ELSE 0 END),0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id LEFT JOIN download_client_status b ON b.provider_id=p.id AND b.media_type=s.media_type AND b.config_revision=p.revision WHERE p.enabled=1 AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=?1",params![media,timestamp]).await.map_err(|_| "storage_error")?.next().await.map_err(|_| "storage_error")?.ok_or("check_failed")?;
    let total: i64 = row.get(0).map_err(|_| "storage_error")?;
    let blocked: i64 = row.get(1).map_err(|_| "storage_error")?;
    if total < 0 || blocked < 0 || blocked > total {
        return Err("check_failed");
    }
    if blocked == 0 {
        return Ok(None);
    }
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity: if blocked == total {
            HealthSeverity::Error
        } else {
            HealthSeverity::Warning
        },
        reason: "download_clients_backoff".into(),
        message: format!(
            "Automatic polling is delayed for {blocked} of {total} enabled download clients after repeated failures. A manual refresh or provider Test can recover the client; new sends remain available."
        ),
        wiki_url: format!(
            "https://wiki.servarr.com/{app}/system#download-clients-are-unavailable-due-to-failures"
        ),
        compatibility_type: "DownloadClientStatusCheck".into(),
    }))
}
