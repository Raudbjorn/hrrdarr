//! Local capability-gap diagnostics for native indexers. Storage reads only: no network I/O.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::db::Database;
use libsql::{TransactionBehavior, params};

const PERSISTENT_FAILURE_SECONDS: i64 = 6 * 60 * 60;

pub(super) async fn evaluate_status(
    db: &Database,
    identity: &HealthIdentity,
    timestamp: i64,
) -> Result<Option<HealthIssue>, &'static str> {
    let (media, app) = match identity.scope {
        HealthScope::Tv => ("tv", "sonarr"),
        HealthScope::Movies => ("movies", "radarr"),
        _ => return Err("check_failed"),
    };
    let persistent = match identity.check_key.as_str() {
        "indexer_status" => false,
        "indexer_failures" => true,
        _ => return Err("check_failed"),
    };
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let row = c.query(
        "SELECT count(*),COALESCE(sum(CASE WHEN b.disabled_until>?2 AND ((?2-b.initial_failure_at>=?3)=?4) THEN 1 ELSE 0 END),0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id LEFT JOIN indexer_status b ON b.provider_id=p.id AND b.media_type=s.media_type AND b.config_revision=p.revision WHERE p.enabled=1 AND p.implementation IN ('torznab','newznab') AND s.media_type=?1 AND (s.enable_rss=1 OR s.enable_automatic_search=1 OR s.enable_interactive_search=1)",
        params![media,timestamp,PERSISTENT_FAILURE_SECONDS,persistent],
    ).await.map_err(|_| "storage_error")?.next().await.map_err(|_| "storage_error")?.ok_or("check_failed")?;
    let total: i64 = row.get(0).map_err(|_| "storage_error")?;
    let unavailable: i64 = row.get(1).map_err(|_| "storage_error")?;
    if total < 0 || unavailable < 0 || unavailable > total {
        return Err("check_failed");
    }
    if unavailable == 0 {
        return Ok(None);
    }
    let (reason, message, compatibility, anchor) = if persistent {
        (
            "indexers_persistent_failures",
            format!(
                "{unavailable} of {total} enabled indexers are unavailable after failures lasting at least six hours. Check the connection and credentials, then Test the provider."
            ),
            "IndexerLongTermStatusCheck",
            "indexers-are-unavailable-due-to-failures",
        )
    } else {
        (
            "indexers_temporary_backoff",
            format!(
                "{unavailable} of {total} enabled indexers are temporarily unavailable after recent failures. They become available when the retry delay expires or a provider Test succeeds."
            ),
            "IndexerStatusCheck",
            "indexers-are-unavailable-due-to-failures",
        )
    };
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity: if unavailable == total {
            HealthSeverity::Error
        } else {
            HealthSeverity::Warning
        },
        reason: reason.into(),
        message,
        wiki_url: format!("https://wiki.servarr.com/{app}/system#{anchor}"),
        compatibility_type: compatibility.into(),
    }))
}

/// Counts use the same eligibility predicate as command admission: master enable,
/// native implementation and the domain scope's per-operation flag.
const COUNTS: &str = "SELECT count(*),COALESCE(sum(s.enable_rss),0),COALESCE(sum(s.enable_automatic_search),0),COALESCE(sum(s.enable_interactive_search),0) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND p.implementation IN ('torznab','newznab','torrentrss') AND s.media_type=?";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::HealthScope;

    async fn feed(c: &libsql::Connection, enabled: i64, rss: i64) {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torrentrss','Feed',?,1,1,1,'http://127.0.0.1:1/feed')", params![id.clone(), enabled]).await.unwrap();
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_rss,enable_automatic_search,enable_interactive_search) VALUES(?,'torrentrss','tv',?,0,0)", params![id, rss]).await.unwrap();
    }
    async fn issue(db: &Database, key: &str) -> Option<String> {
        evaluate(
            db,
            &HealthIdentity {
                scope: HealthScope::Tv,
                check_key: key.into(),
            },
        )
        .await
        .unwrap()
        .map(|i| i.reason)
    }

    // Reasoning: a feed counts as an enabled indexer for capability reporting, but its search flags are
    // storage-forced to zero, so a feed-only library still reports that no search exists.
    #[tokio::test]
    async fn feeds_count_for_rss_but_never_for_search() {
        let dir =
            std::env::temp_dir().join(format!("hrrdarr-feed-health-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let db = Database::open_local(dir.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        assert_eq!(
            issue(&db, "indexer_search").await.as_deref(),
            Some("indexers_none_enabled")
        );
        feed(&c, 1, 1).await;
        assert_eq!(issue(&db, "indexer_rss").await, None);
        assert_eq!(
            issue(&db, "indexer_search").await.as_deref(),
            Some("indexer_automatic_search_unavailable")
        );
        c.execute("UPDATE provider_scopes SET enable_rss=0", ())
            .await
            .unwrap();
        assert_eq!(
            issue(&db, "indexer_rss").await.as_deref(),
            Some("indexer_rss_unavailable")
        );
        c.execute("UPDATE providers SET enabled=0,revision=revision+1", ())
            .await
            .unwrap();
        assert_eq!(
            issue(&db, "indexer_search").await.as_deref(),
            Some("indexers_none_enabled")
        );
        drop(c);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
