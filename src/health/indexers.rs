//! Local capability-gap diagnostics for native indexers. Storage reads only: no network I/O.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::db::Database;
use libsql::{TransactionBehavior, params};

/// Counts use the same eligibility predicate as command admission: master enable,
/// native implementation and the domain scope's per-operation flag.
const COUNTS: &str = "SELECT count(*),COALESCE(sum(s.enable_rss),0),COALESCE(sum(s.enable_automatic_search),0),COALESCE(sum(s.enable_interactive_search),0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND p.implementation IN ('torznab','newznab') AND s.media_type=?";

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>, &'static str> {
    let (media, sonarr) = match identity.scope {
        HealthScope::Tv => ("tv", true),
        HealthScope::Movies => ("movies", false),
        _ => return Err("check_failed"),
    };
    let compatibility = match identity.check_key.as_str() {
        "indexer_search" => "IndexerSearchCheck",
        "indexer_rss" => "IndexerRssCheck",
        _ => return Err("check_failed"),
    };
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    let row = tx
        .query(COUNTS, params![media])
        .await
        .map_err(|_| "storage_error")?
        .next()
        .await
        .map_err(|_| "storage_error")?
        .ok_or("check_failed")?;
    let [total, rss, automatic, interactive] =
        [0, 1, 2, 3].map(|i| row.get::<i64>(i).map_err(|_| "storage_error"));
    let (total, rss, automatic, interactive) = (total?, rss?, automatic?, interactive?);
    drop(row);
    tx.commit().await.map_err(|_| "storage_error")?;
    if [total, rss, automatic, interactive]
        .iter()
        .any(|v| *v < 0 || *v > total)
    {
        return Err("check_failed");
    }
    let app = if sonarr { "sonarr" } else { "radarr" };
    let (severity, reason, message, anchor) = match identity.check_key.as_str() {
        // The search key reports the most severe gap only; RSS reports separately, and only
        // when some indexer is enabled so a fully absent configuration is not double-reported.
        "indexer_search" if total == 0 => (
            HealthSeverity::Error,
            "indexers_none_enabled",
            "No indexer is enabled for this library; no search or RSS release source is available.",
            "indexers-are-unavailable",
        ),
        "indexer_search" if automatic == 0 => (
            HealthSeverity::Error,
            "indexer_automatic_search_unavailable",
            "No enabled indexer has automatic search enabled; background searches will find no releases.",
            "no-indexers-available-with-automatic-search-enabled",
        ),
        "indexer_search" if interactive == 0 => (
            HealthSeverity::Warning,
            "indexer_interactive_search_unavailable",
            "No enabled indexer has interactive search enabled; manual searches will find no releases.",
            "no-indexers-available-with-interactive-search-enabled",
        ),
        "indexer_rss" if total > 0 && rss == 0 => (
            HealthSeverity::Warning,
            "indexer_rss_unavailable",
            "No enabled indexer has RSS sync enabled; new releases will not be grabbed from feeds.",
            "no-indexers-available-with-rss-sync-enabled",
        ),
        _ => return Ok(None),
    };
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity,
        reason: reason.into(),
        message: message.into(),
        wiki_url: format!("https://wiki.servarr.com/{app}/system#{anchor}"),
        compatibility_type: compatibility.into(),
    }))
}
