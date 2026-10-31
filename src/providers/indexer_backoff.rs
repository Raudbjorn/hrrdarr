//! Scoped, revision-fenced indexer observations. Admission never changes failure history.
use super::*;

const MAX_SECONDS: i64 = 86_400;
const LONG_TERM_SECONDS: i64 = 21_600;
const MAX_INTEGER: i64 = 9_007_199_254_740_991;
const BACKOFF_SECONDS: [i64; 9] = [60, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400];

#[derive(Clone, Copy)]
pub(super) enum Observation {
    Success,
    Failure { minimum_seconds: u32 },
}

pub(super) fn observation<T>(
    result: &std::result::Result<T, indexer::IndexerError>,
) -> Option<Observation> {
    use http::HttpError as H;
    use indexer::IndexerError as I;
    match result {
        Ok(_) => Some(Observation::Success),
        Err(
            I::InvalidRequest
            | I::Unsupported
            | I::Transport(H::InvalidRequest | H::LocalUnavailable { .. } | H::Busy),
        ) => None,
        Err(
            I::RateLimited {
                retry_after_seconds,
            }
            | I::Transport(H::RateLimited {
                retry_after_seconds,
            }),
        ) => Some(Observation::Failure {
            minimum_seconds: retry_after_seconds.unwrap_or(60).clamp(1, 86400),
        }),
        Err(_) => Some(Observation::Failure { minimum_seconds: 0 }),
    }
}

pub(super) fn domain(request: &indexer::IndexerSearch) -> MediaDomain {
    match request {
        indexer::IndexerSearch::Rss { media_type, .. } => *media_type,
        indexer::IndexerSearch::Tv { .. } => MediaDomain::Tv,
        indexer::IndexerSearch::Movie { .. } => MediaDomain::Movies,
    }
}

pub(super) fn has_scope(settings: &ProviderSettings, media: MediaDomain) -> bool {
    match (settings, media) {
        (
            ProviderSettings::Torznab { tv, .. } | ProviderSettings::Newznab { tv, .. },
            MediaDomain::Tv,
        ) => tv.is_some(),
        (
            ProviderSettings::Torznab { movies, .. } | ProviderSettings::Newznab { movies, .. },
            MediaDomain::Movies,
        ) => movies.is_some(),
        // Trackers share the durable backoff with Torznab/Newznab, scoped per configured domain.
        (settings, media) => match (settings.tracker_parts(), media) {
            (Some((_, tv, _)), MediaDomain::Tv) => tv.is_some(),
            (Some((_, _, movies)), MediaDomain::Movies) => movies.is_some(),
            _ => false,
        },
    }
}

fn media_name(media: MediaDomain) -> &'static str {
    match media {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}

pub(super) fn now() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .filter(|v| *v <= MAX_INTEGER - MAX_SECONDS)
        .ok_or_else(corrupt)
}

pub(super) async fn admit(db: &Database, provider: &Provider, media: MediaDomain) -> Result<()> {
    // A plain RSS feed has no durable backoff authority (unlike Torznab/Newznab and trackers);
    // it keeps the transport's in-memory rate-limit cooldown instead.
    if matches!(provider.settings, ProviderSettings::Torrentrss { .. }) {
        return Ok(());
    }
    let c = db.connect().await?;
    let row = c.query("SELECT disabled_until FROM indexer_status WHERE provider_id=? AND media_type=? AND config_revision=?", params![provider.id.to_string(),media_name(media),provider.revision]).await?.next().await?;
    if let Some(row) = row {
        let remaining = row.get::<i64>(0)? - now()?;
        if remaining > 0 {
            return Err(Error::RateLimited(remaining.min(MAX_SECONDS) as u32));
        }
    }
    Ok(())
}

pub(super) async fn record(
    db: &Database,
    provider: &Provider,
    media: MediaDomain,
    observation: Option<Observation>,
) -> Result<()> {
    // A plain RSS feed has no durable backoff authority (unlike Torznab/Newznab and trackers);
    // it keeps the transport's in-memory rate-limit cooldown instead.
    if matches!(provider.settings, ProviderSettings::Torrentrss { .. }) {
        return Ok(());
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    apply(&tx, provider, media, observation, now()?).await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn apply(
    c: &Connection,
    provider: &Provider,
    media: MediaDomain,
    observation: Option<Observation>,
    timestamp: i64,
) -> Result<()> {
    // A plain RSS feed has no durable backoff authority (unlike Torznab/Newznab and trackers);
    // it keeps the transport's in-memory rate-limit cooldown instead.
    if matches!(provider.settings, ProviderSettings::Torrentrss { .. }) {
        return Ok(());
    }
    if c.is_autocommit() {
        return Err(corrupt());
    }
    if c.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND s.media_type=?",params![provider.id.to_string(),provider.revision,media_name(media)]).await?.next().await?.is_none() { return Err(conflict()); }
    let Some(observation) = observation else {
        return Ok(());
    };
    let changed = match observation {
        Observation::Success => c.execute("DELETE FROM indexer_status WHERE provider_id=? AND media_type=? AND config_revision=?",params![provider.id.to_string(),media_name(media),provider.revision]).await? > 0,
        Observation::Failure { minimum_seconds } => {
            let previous = c.query("SELECT failure_count,initial_failure_at,last_failure_at FROM indexer_status WHERE provider_id=? AND media_type=? AND config_revision=?",params![provider.id.to_string(),media_name(media),provider.revision]).await?.next().await?;
            let (count, initial, timestamp) = match previous {
                Some(row) => ((row.get::<i64>(0)? + 1).min(BACKOFF_SECONDS.len() as i64),row.get::<i64>(1)?,timestamp.max(row.get::<i64>(2)?)),
                None => (1,timestamp,timestamp),
            };
            let duration = BACKOFF_SECONDS[(count - 1) as usize].max(i64::from(minimum_seconds)).min(MAX_SECONDS);
            let until = timestamp.checked_add(duration).filter(|v| *v <= MAX_INTEGER).ok_or_else(corrupt)?;
            let threshold = initial.checked_add(LONG_TERM_SECONDS).ok_or_else(corrupt)?;
            let next_health_at = if threshold > timestamp { until.min(threshold) } else { until };
            c.execute("INSERT INTO indexer_status(provider_id,media_type,config_revision,failure_count,initial_failure_at,last_failure_at,disabled_until,next_health_at) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(provider_id,media_type) DO UPDATE SET config_revision=excluded.config_revision,failure_count=excluded.failure_count,initial_failure_at=excluded.initial_failure_at,last_failure_at=excluded.last_failure_at,disabled_until=excluded.disabled_until,next_health_at=excluded.next_health_at",params![provider.id.to_string(),media_name(media),provider.revision,count,initial,timestamp,until,next_health_at]).await?;
            true
        }
    };
    if changed {
        crate::health::indexer_status_changed(c, media)
            .await
            .map_err(|e| {
                Error::Plain(e.0, e.1, "Indexer health observation could not be recorded")
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    async fn fixture(endpoint: &str) -> (Scratch, Arc<Database>, Provider) {
        let path = std::env::temp_dir().join(format!("indexer-backoff-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        let id = Uuid::new_v4().to_string();
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Backoff fixture',1,1,1,1,?)",params![id.clone(),endpoint]).await.unwrap();
        let settings = serde_json::from_value(serde_json::json!({"implementation":"torznab","endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}})).unwrap();
        write_scopes(&c, &id, &settings).await.unwrap();
        let provider = read(&c, &id).await.unwrap().0;
        (Scratch(path), db, provider)
    }
    #[test]
    fn only_attributable_failures_change_backoff() {
        use http::HttpError as H;
        use indexer::IndexerError as I;
        for error in [
            I::InvalidRequest,
            I::Unsupported,
            I::Transport(H::Busy),
            I::Transport(H::InvalidRequest),
            I::Transport(H::LocalUnavailable { timeout: true }),
            I::Transport(H::LocalUnavailable { timeout: false }),
        ] {
            assert!(observation(&Err::<(), _>(error)).is_none());
        }
        for error in [
            I::Authentication,
            I::InvalidResponse,
            I::Transport(H::Transport),
            I::Transport(H::Timeout),
            I::Transport(H::Redirect),
            I::Transport(H::ResponseTooLarge),
        ] {
            assert!(matches!(
                observation(&Err::<(), _>(error)),
                Some(Observation::Failure { .. })
            ));
        }
        assert!(matches!(
            observation(&Err::<(), _>(I::RateLimited {
                retry_after_seconds: Some(u32::MAX)
            })),
            Some(Observation::Failure {
                minimum_seconds: 86400
            })
        ));
    }
    #[tokio::test]
    async fn durable_escalation_is_bounded_scoped_atomic_and_revision_fenced() {
        let (_scratch, db, provider) = fixture("http://127.0.0.1:1/").await;
        let c = db.connect().await.unwrap();
        for n in 1..=12 {
            let tx = c
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await
                .unwrap();
            apply(
                &tx,
                &provider,
                MediaDomain::Tv,
                Some(Observation::Failure { minimum_seconds: 0 }),
                100 + n,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            let row=c.query("SELECT failure_count,initial_failure_at,last_failure_at,disabled_until,next_health_at FROM indexer_status",()).await.unwrap().next().await.unwrap().unwrap();
            assert_eq!(row.get::<i64>(0).unwrap(), n.min(9));
            assert_eq!(row.get::<i64>(1).unwrap(), 101);
            assert_eq!(
                row.get::<i64>(3).unwrap() - row.get::<i64>(2).unwrap(),
                BACKOFF_SECONDS[(n.min(9) - 1) as usize]
            );
            assert_eq!(
                row.get::<i64>(4).unwrap(),
                row.get::<i64>(3).unwrap().min(101 + 21600)
            );
        }
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        apply(
            &tx,
            &provider,
            MediaDomain::Movies,
            Some(Observation::Failure {
                minimum_seconds: 7200,
            }),
            200,
        )
        .await
        .unwrap();
        apply(
            &tx,
            &provider,
            MediaDomain::Tv,
            Some(Observation::Success),
            200,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let row = c
            .query("SELECT media_type,disabled_until FROM indexer_status", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "movies");
        assert_eq!(row.get::<i64>(1).unwrap(), 7400);
        c.execute(
            "UPDATE providers SET revision=2 WHERE id=?",
            [provider.id.to_string()],
        )
        .await
        .unwrap();
        assert!(
            c.query("SELECT 1 FROM indexer_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            record(
                &db,
                &provider,
                MediaDomain::Tv,
                Some(Observation::Failure { minimum_seconds: 0 })
            )
            .await
            .is_err()
        );
        assert!(
            c.query("SELECT 1 FROM indexer_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn raw_gateway_keeps_backoff_across_restart_and_expires_without_cross_scope_requests() {
        let failure = Arc::new(AtomicBool::new(true));
        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let state = (failure.clone(), requests.clone());
        let app=axum::Router::new().route("/api",axum::routing::get(|axum::extract::State((failure,requests)):axum::extract::State<(Arc<AtomicBool>,Arc<AtomicUsize>)>, axum::extract::OriginalUri(uri):axum::extract::OriginalUri| async move {
            requests.fetch_add(1,Ordering::SeqCst);
            if failure.load(Ordering::SeqCst) { return (StatusCode::SERVICE_UNAVAILABLE,"offline"); }
            if uri.query().unwrap_or("").contains("t=caps") { (StatusCode::OK,r#"<caps><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,season,ep"/><movie-search available="yes" supportedParams="q"/></searching><categories><category id="5000"/><category id="2000"/></categories></caps>"#) }
            else { (StatusCode::OK,"<rss><channel/></rss>") }
        })).with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_scratch, db, provider) = fixture(&endpoint).await;
        let (_, client) = router_with_refresh(db.clone(), None);
        let query = |media| indexer::IndexerSearch::Rss {
            media_type: media,
            offset: 0,
            query_index: 0,
            limit: 1,
        };
        assert!(
            client
                .raw_search(
                    &provider.id.to_string(),
                    1,
                    IndexerOperation::Rss,
                    &query(MediaDomain::Tv)
                )
                .await
                .is_err()
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let (_, client) = router_with_refresh(db.clone(), None);
        let blocked = client
            .raw_search(
                &provider.id.to_string(),
                1,
                IndexerOperation::Rss,
                &query(MediaDomain::Tv),
            )
            .await
            .err()
            .unwrap();
        assert!(blocked.retry_after_seconds.unwrap() > 0);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        failure.store(false, Ordering::SeqCst);
        client
            .raw_search(
                &provider.id.to_string(),
                1,
                IndexerOperation::Rss,
                &query(MediaDomain::Movies),
            )
            .await
            .expect("eligible scope fetch succeeds after the fixture recovers");
        let c = db.connect().await.unwrap();
        c.execute("UPDATE indexer_status SET initial_failure_at=0,last_failure_at=0,disabled_until=?,next_health_at=NULL",[now().unwrap()-1]).await.unwrap();
        client
            .raw_search(
                &provider.id.to_string(),
                1,
                IndexerOperation::Rss,
                &query(MediaDomain::Tv),
            )
            .await
            .expect("eligible scope fetch succeeds after the fixture recovers");
        assert!(
            c.query("SELECT 1 FROM indexer_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn cancelled_network_work_releases_lane_without_publishing_failure() {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .route(
                "/api",
                axum::routing::get(
                    |axum::extract::State((started, release)): axum::extract::State<(
                        Arc<tokio::sync::Notify>,
                        Arc<tokio::sync::Notify>,
                    )>| async move {
                        started.notify_one();
                        release.notified().await;
                        (StatusCode::SERVICE_UNAVAILABLE, "offline")
                    },
                ),
            )
            .with_state((started.clone(), release.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (_scratch, db, provider) = fixture(&endpoint).await;
        let (_, client) = router_with_refresh(db.clone(), None);
        let pending_client = client.clone();
        let id = provider.id.to_string();
        let pending = tokio::spawn(async move {
            pending_client
                .raw_search(
                    &id,
                    1,
                    IndexerOperation::Rss,
                    &indexer::IndexerSearch::Rss {
                        media_type: MediaDomain::Tv,
                        offset: 0,
                        query_index: 0,
                        limit: 1,
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        pending.abort();
        assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
        let c = db.connect().await.unwrap();
        assert!(
            c.query("SELECT 1 FROM indexer_status", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .is_none()
        );
        let transport = client.0.transport.as_ref().as_ref().unwrap();
        assert!(
            transport.indexer_operation(provider.id).is_ok(),
            "cancelled future releases the provider permit"
        );
        release.notify_one();
        server.abort();
        let _ = server.await;
    }
}
