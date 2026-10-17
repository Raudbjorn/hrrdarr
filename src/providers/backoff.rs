//! Download-client failure history; only accepted observations mutate this authority.
use super::*;

const INITIAL_GRACE_SECONDS: i64 = 300;
const MAX_INTEGER: i64 = 9_007_199_254_740_991;
const DELAYS_SECONDS: [i64; 6] = [0, 60, 300, 900, 1800, 3600];
pub(super) const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(900);

#[derive(Clone, Copy)]
pub(crate) enum Observation {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Status {
    level: i64,
    initial: Option<i64>,
    last: Option<i64>,
    until: Option<i64>,
}

fn transition(
    mut status: Status,
    observation: Observation,
    timestamp: i64,
    grace_elapsed: bool,
) -> std::result::Result<Status, ()> {
    if !(0..=MAX_INTEGER).contains(&timestamp) || status.last.is_some_and(|last| timestamp < last) {
        return Err(());
    }
    match observation {
        Observation::Success => {
            if status.level > 0 {
                status.level -= 1;
                status.until = None;
            }
        }
        Observation::Failure => {
            if status.level == 0 {
                status.level = 1;
                status.initial = Some(timestamp);
            } else if timestamp - status.initial.ok_or(())? >= INITIAL_GRACE_SECONDS
                && grace_elapsed
            {
                status.level = (status.level + 1).min(5);
            }
            status.last = Some(timestamp);
            if timestamp - status.initial.ok_or(())? >= INITIAL_GRACE_SECONDS {
                status.until = Some(
                    timestamp
                        .checked_add(DELAYS_SECONDS[status.level as usize])
                        .filter(|until| *until <= MAX_INTEGER)
                        .ok_or(())?,
                );
            }
            if !grace_elapsed && let Some(until) = status.until {
                status.until = Some(
                    until.min(
                        timestamp
                            .checked_add(INITIAL_GRACE_SECONDS)
                            .filter(|v| *v <= MAX_INTEGER)
                            .ok_or(())?,
                    ),
                );
            }
        }
    }
    Ok(status)
}

pub(crate) async fn apply(
    c: &Connection,
    id: &str,
    revision: i64,
    domain: MediaDomain,
    observation: Observation,
    timestamp: i64,
    grace_elapsed: bool,
) -> Result<()> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    if c.query("SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client') AND s.media_type=?",params![id,revision,media]).await?.next().await?.is_none() { return Err(conflict()); }
    let previous = c.query("SELECT escalation_level,initial_failure_at,last_failure_at,disabled_until FROM download_client_status WHERE provider_id=? AND media_type=? AND config_revision=?",params![id,media,revision]).await?.next().await?
        .map(|row| -> Result<Status> { Ok(Status {level:row.get(0)?,initial:row.get(1)?,last:row.get(2)?,until:row.get(3)?}) }).transpose()?.unwrap_or_default();
    let Ok(updated) = transition(previous, observation, timestamp, grace_elapsed) else {
        // Preserve the accepted remote fact and prior authority when the wall clock cannot represent policy.
        c.execute(
            "UPDATE health_lifecycle SET schedule_error='health_invariant' WHERE id=1",
            (),
        )
        .await?;
        crate::health::client_backoff_changed(c, domain)
            .await
            .map_err(|e| Error::Plain(e.0, e.1, "Client backoff health could not be recorded"))?;
        return Ok(());
    };
    if previous == updated {
        return Ok(());
    }
    c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(provider_id,media_type) DO UPDATE SET config_revision=excluded.config_revision,escalation_level=excluded.escalation_level,initial_failure_at=excluded.initial_failure_at,last_failure_at=excluded.last_failure_at,disabled_until=excluded.disabled_until,next_health_at=excluded.next_health_at",params![id,media,revision,updated.level,updated.initial,updated.last,updated.until,updated.until]).await?;
    crate::health::client_backoff_changed(c, domain)
        .await
        .map_err(|e| Error::Plain(e.0, e.1, "Client backoff health could not be recorded"))?;
    Ok(())
}

/// Scope replacement is temporary; reconcile only after the final configuration has been written.
pub(super) async fn reconcile(c: &Connection, id: &str) -> Result<()> {
    if c.is_autocommit() {
        return Err(corrupt());
    }
    c.execute("DELETE FROM download_client_status WHERE provider_id=? AND NOT EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=download_client_status.provider_id AND s.media_type=download_client_status.media_type AND s.implementation IN (SELECT implementation FROM provider_implementations WHERE kind='download_client'))",[id]).await?;
    c.execute("UPDATE download_client_status SET config_revision=(SELECT revision FROM providers WHERE id=?) WHERE provider_id=?",params![id,id]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_and_startup_grace_differ_and_success_recovers_one_level() {
        let first = transition(Status::default(), Observation::Failure, 1000, false).unwrap();
        assert_eq!(
            first,
            Status {
                level: 1,
                initial: Some(1000),
                last: Some(1000),
                until: None
            }
        );
        let early = transition(first, Observation::Failure, 1299, true).unwrap();
        assert_eq!(early.level, 1);
        assert_eq!(early.until, None);
        let startup = transition(early, Observation::Failure, 1300, false).unwrap();
        assert_eq!(startup.level, 1);
        assert_eq!(
            startup.until,
            Some(1360),
            "startup grace still permits a block after initial grace"
        );
        let mut status = startup;
        for (timestamp, level, delay) in [
            (1900, 2, 300),
            (2200, 3, 900),
            (3100, 4, 1800),
            (4900, 5, 3600),
            (8500, 5, 3600),
        ] {
            status = transition(status, Observation::Failure, timestamp, true).unwrap();
            assert_eq!(
                (status.level, status.until),
                (level, Some(timestamp + delay))
            );
            assert_eq!(status.initial, Some(1000));
        }
        status = transition(status, Observation::Failure, 12100, false).unwrap();
        assert_eq!(
            (status.level, status.until),
            (5, Some(12400)),
            "restart startup grace caps an existing high level block at five minutes"
        );
        for level in (0..5).rev() {
            status = transition(status, Observation::Success, 12101, true).unwrap();
            assert_eq!(status.level, level);
            assert_eq!(status.until, None);
            assert_eq!((status.initial, status.last), (Some(1000), Some(12100)));
        }
        assert_eq!(
            transition(status, Observation::Success, 12101, true).unwrap(),
            status
        );
        let restarted = transition(status, Observation::Failure, 13000, true).unwrap();
        assert_eq!(
            (restarted.level, restarted.initial, restarted.until),
            (1, Some(13000), None)
        );
        assert!(transition(restarted, Observation::Failure, 12999, true).is_err());
        assert!(transition(restarted, Observation::Failure, MAX_INTEGER, true).is_err());
    }

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    async fn state(
        c: &Connection,
        id: &str,
    ) -> Vec<(
        String,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    )> {
        let mut rows=c.query("SELECT media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until,next_health_at FROM download_client_status WHERE provider_id=? ORDER BY media_type",[id]).await.unwrap();
        let mut values = vec![];
        while let Some(row) = rows.next().await.unwrap() {
            values.push((
                row.get(0).unwrap(),
                row.get(1).unwrap(),
                row.get(2).unwrap(),
                row.get(3).unwrap(),
                row.get(4).unwrap(),
                row.get(5).unwrap(),
                row.get(6).unwrap(),
            ));
        }
        values
    }
    #[tokio::test]
    async fn accepted_client_backoff_preserves_history_rebinds_final_scopes_and_fences_stale_results()
     {
        let path = std::env::temp_dir().join(format!("hrrdarr-client-backoff-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let db = Database::open_local(path.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let id = Uuid::new_v4().to_string();
        let mut settings:ProviderSettings=serde_json::from_value(serde_json::json!({"implementation":"qbittorrent","endpoint":"http://127.0.0.1:1","tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}})).unwrap();
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Backoff fixture',1,1,1,1,'http://127.0.0.1:1')",[id.clone()]).await.unwrap();
        write_scopes(&tx, &id, &settings).await.unwrap();
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            apply(&tx, &id, 1, domain, Observation::Failure, 1000, true)
                .await
                .unwrap();
            apply(&tx, &id, 1, domain, Observation::Failure, 1300, true)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let accepted = state(&c, &id).await;
        assert_eq!(accepted.len(), 2);
        assert!(
            accepted
                .iter()
                .all(|r| r.2 == 2 && r.5 == Some(1600) && r.6 == Some(1600))
        );
        assert!(
            apply(
                &c,
                &id,
                1,
                MediaDomain::Tv,
                Observation::Failure,
                1600,
                true
            )
            .await
            .is_err(),
            "caller must own transaction"
        );
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        apply(
            &tx,
            &id,
            1,
            MediaDomain::Tv,
            Observation::Success,
            1301,
            true,
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(
            state(&c, &id).await,
            accepted,
            "outcome rollback preserves status"
        );
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        tx.execute(
            "UPDATE providers SET revision=2,enabled=0,endpoint='http://127.0.0.1:2' WHERE id=?",
            [id.clone()],
        )
        .await
        .unwrap();
        write_scopes(&tx, &id, &settings).await.unwrap();
        tx.commit().await.unwrap();
        let rebound = state(&c, &id).await;
        assert!(rebound.iter().all(|r| r.1 == 2
            && r.2 == 2
            && r.3 == Some(1000)
            && r.4 == Some(1300)
            && r.5 == Some(1600)));
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        assert!(
            apply(
                &tx,
                &id,
                1,
                MediaDomain::Tv,
                Observation::Success,
                1301,
                true
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();
        assert_eq!(state(&c, &id).await, rebound);
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        apply(
            &tx,
            &id,
            2,
            MediaDomain::Tv,
            Observation::Failure,
            1299,
            true,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            state(&c, &id).await,
            rebound,
            "clock reversal retains accepted history"
        );
        assert_eq!(
            c.query("SELECT schedule_error FROM health_lifecycle WHERE id=1", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "health_invariant"
        );
        if let ProviderSettings::Qbittorrent { movies, .. } = &mut settings {
            *movies = None;
        }
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        tx.execute("UPDATE providers SET revision=3 WHERE id=?", [id.clone()])
            .await
            .unwrap();
        write_scopes(&tx, &id, &settings).await.unwrap();
        tx.commit().await.unwrap();
        let remaining = state(&c, &id).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            (&remaining[0].0, remaining[0].1, remaining[0].2),
            (&"tv".to_string(), 3, 2)
        );
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .unwrap();
        apply(
            &tx,
            &id,
            3,
            MediaDomain::Tv,
            Observation::Success,
            1301,
            true,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let recovered = state(&c, &id).await;
        assert_eq!(
            (recovered[0].2, recovered[0].5, recovered[0].6),
            (1, None, None)
        );
        drop(c);
        drop(db);
        let reopened = Database::open_local(path.join("db")).await.unwrap();
        assert_eq!(
            state(&reopened.connect().await.unwrap(), &id).await,
            recovered
        );
    }
}
