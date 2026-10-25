//! Outbox fan-out and delivery. At-least-once: a delivery is claimed with a lease, resolved after
//! the send, and re-claimable if the process dies in between. Retries back off per delivery; a
//! permanent failure or exhausted attempts parks it as `failed` and never blocks other targets.
use super::{
    catalog::{self, Build, Config},
    events::{self, EventKind, EventRow, Message, Scope},
    model::TestStatus,
    script,
    store::{Runtime, Store},
    transport::{SendError, Transport},
};
use crate::{api::MediaDomain, db::Database, providers::CredentialKey};
use libsql::{Connection, params};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::{Notify, Semaphore};

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
const FAN_OUT_BATCH: i64 = 64;
const DELIVERY_BATCH: i64 = 16;
const LEASE_SECONDS: i64 = 120;
pub const MAX_ATTEMPTS: i64 = 5;
const BACKOFF_BASE_SECONDS: i64 = 30;
const BACKOFF_CAP_SECONDS: i64 = 6 * 60 * 60;
const MAX_CONCURRENT_DELIVERIES: usize = 4;
const MAX_CONCURRENT_SCRIPTS: usize = 2;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}
/// 30s, 2m, 8m, 32m, ... capped at 6h; a server-supplied Retry-After may lengthen it.
pub fn backoff_seconds(attempts: i64) -> i64 {
    let exp = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(10);
    BACKOFF_BASE_SECONDS
        .saturating_mul(4i64.saturating_pow(exp))
        .min(BACKOFF_CAP_SECONDS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryError {
    pub code: &'static str,
    pub retryable: bool,
    pub retry_after: Option<u32>,
}
impl From<SendError> for DeliveryError {
    fn from(e: SendError) -> Self {
        Self {
            code: e.code(),
            retryable: e.retryable(),
            retry_after: e.retry_after_seconds(),
        }
    }
}
impl From<script::ScriptError> for DeliveryError {
    fn from(e: script::ScriptError) -> Self {
        Self {
            code: e.code(),
            retryable: false,
            retry_after: None,
        }
    }
}
fn permanent(code: &'static str) -> DeliveryError {
    DeliveryError {
        code,
        retryable: false,
        retry_after: None,
    }
}

#[derive(Clone)]
pub struct Dispatcher {
    pub store: Arc<Store>,
    pub transport: Arc<Transport>,
    scripts: Arc<Semaphore>,
}

impl Dispatcher {
    pub fn new(db: Arc<Database>, key: Option<Arc<CredentialKey>>, transport: Transport) -> Self {
        Self {
            store: Arc::new(Store { db, key }),
            transport: Arc::new(transport),
            scripts: Arc::new(Semaphore::new(MAX_CONCURRENT_SCRIPTS)),
        }
    }

    /// Performs the I/O for one message. `Ok(false)` means the implementation had nothing to send.
    pub async fn send(&self, rt: &Runtime, msg: &Message) -> Result<bool, DeliveryError> {
        let config = Config {
            settings: &rt.notification.settings,
            secrets: &rt.secrets,
        };
        enum Plan {
            Http(Vec<super::transport::Outgoing>),
            Plex,
            Script,
        }
        // Resolved synchronously so no fn-pointer type is held across an await point.
        let plan = {
            let implementation = catalog::find(&rt.notification.implementation)
                .ok_or_else(|| permanent("unknown_implementation"))?;
            match implementation.build {
                Build::Http(build) => Plan::Http(build(&config, msg).map_err(permanent)?),
                Build::Plex => Plan::Plex,
                Build::Script => Plan::Script,
            }
        };
        match plan {
            Plan::Http(requests) => {
                if requests.is_empty() {
                    return Ok(false);
                }
                for request in &requests {
                    self.transport.send(request).await?;
                }
                Ok(true)
            }
            Plan::Plex => Ok(catalog_plex(&self.transport, &config, msg).await?),
            Plan::Script => {
                let path = config.text("path").to_owned();
                let args = config.text("arguments").to_owned();
                let env = msg.env.clone();
                let _permit = self
                    .scripts
                    .acquire()
                    .await
                    .map_err(|_| permanent("script_unavailable"))?;
                tokio::task::spawn_blocking(move || script::run_blocking(&path, &args, &env))
                    .await
                    .map_err(|_| permanent("script_failed"))?
                    .map_err(DeliveryError::from)?;
                Ok(true)
            }
        }
    }

    /// Sends a synthetic Test event for every configured domain of a runtime definition.
    pub async fn test(&self, rt: &Runtime) -> Vec<super::model::DomainTestResult> {
        let mut out = Vec::new();
        let mut domains = Vec::new();
        if rt.notification.tv.is_some() {
            domains.push(MediaDomain::Tv);
        }
        if rt.notification.movies.is_some() {
            domains.push(MediaDomain::Movies);
        }
        for domain in domains {
            let msg = events::test_message(domain);
            let (status, error) =
                match tokio::time::timeout(Duration::from_secs(40), self.send(rt, &msg)).await {
                    Ok(Ok(true)) => (TestStatus::Delivered, None),
                    Ok(Ok(false)) => (TestStatus::Skipped, None),
                    Ok(Err(e)) => (TestStatus::Failed, Some(e.code.to_owned())),
                    Err(_) => (TestStatus::Failed, Some("timeout".into())),
                };
            out.push(super::model::DomainTestResult {
                media_type: domain,
                status,
                error,
            });
        }
        out
    }

    /// Turns unprocessed events into per-notification delivery rows.
    pub async fn fan_out(&self) -> Result<usize, libsql::Error> {
        let c = self.store.db.connect().await?;
        let mut pending = Vec::new();
        {
            let mut rows = c
                .query(
                    "SELECT sequence,kind,scope,subject_id,payload_json,tag_ids_json FROM notification_events WHERE fanned_out=0 ORDER BY sequence LIMIT ?",
                    [FAN_OUT_BATCH],
                )
                .await?;
            while let Some(r) = rows.next().await? {
                let kind = EventKind::parse(&r.get::<String>(1)?);
                let scope = Scope::parse(&r.get::<String>(2)?);
                let payload: Option<Value> = serde_json::from_str(&r.get::<String>(4)?).ok();
                let tags: Option<Vec<i64>> = r
                    .get::<Option<String>>(5)?
                    .and_then(|t| serde_json::from_str(&t).ok());
                pending.push((
                    r.get::<i64>(0)?,
                    kind.zip(scope)
                        .zip(payload)
                        .map(|((kind, scope), payload)| EventRow {
                            sequence: 0,
                            kind,
                            scope,
                            subject_id: r.get::<Option<i64>>(3).ok().flatten(),
                            payload,
                            tag_ids: tags,
                        }),
                ));
            }
        }
        let mut created = 0;
        for (sequence, event) in pending {
            let targets = match event {
                Some(mut event) => {
                    event.sequence = sequence;
                    matching(&c, &event).await?
                }
                None => Vec::new(), // unreadable row: mark processed rather than loop on it
            };
            let tx = c
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await?;
            let t = now();
            for id in &targets {
                created += tx
                    .execute(
                        "INSERT OR IGNORE INTO notification_deliveries(event_sequence,notification_id,next_attempt_at,created_at) VALUES(?,?,?,?)",
                        params![sequence, id.clone(), t, t],
                    )
                    .await? as usize;
            }
            tx.execute(
                "UPDATE notification_events SET fanned_out=1 WHERE sequence=?",
                [sequence],
            )
            .await?;
            tx.commit().await?;
        }
        Ok(created)
    }

    /// Claims and attempts due deliveries; returns how many were resolved or rescheduled.
    pub async fn deliver_due(&self) -> Result<usize, libsql::Error> {
        let c = self.store.db.connect().await?;
        let t = now();
        let mut claimed = Vec::new();
        {
            let mut rows = c
                .query(
                    "SELECT id,event_sequence,notification_id,attempts FROM notification_deliveries WHERE state='pending' AND next_attempt_at<=?1 AND lease_until<=?1 ORDER BY next_attempt_at,id LIMIT ?2",
                    params![t, DELIVERY_BATCH],
                )
                .await?;
            while let Some(r) = rows.next().await? {
                claimed.push((
                    r.get::<i64>(0)?,
                    r.get::<i64>(1)?,
                    r.get::<String>(2)?,
                    r.get::<i64>(3)?,
                ));
            }
        }
        let gate = Arc::new(Semaphore::new(MAX_CONCURRENT_DELIVERIES));
        let mut tasks = tokio::task::JoinSet::new();
        for (id, sequence, notification, attempts) in claimed {
            let lease = c
                .execute(
                    "UPDATE notification_deliveries SET lease_until=?1 WHERE id=?2 AND state='pending' AND lease_until<=?3",
                    params![t + LEASE_SECONDS, id, t],
                )
                .await?;
            if lease == 0 {
                continue;
            }
            let this = self.clone();
            let gate = gate.clone();
            tasks.spawn(async move {
                let Ok(_permit) = gate.acquire().await else {
                    return false;
                };
                this.attempt(id, sequence, &notification, attempts)
                    .await
                    .is_ok()
            });
        }
        let mut done = 0;
        while let Some(result) = tasks.join_next().await {
            if result.is_ok() {
                done += 1;
            }
        }
        Ok(done)
    }

    async fn attempt(
        &self,
        id: i64,
        sequence: i64,
        notification: &str,
        attempts: i64,
    ) -> Result<(), libsql::Error> {
        let c = self.store.db.connect().await?;
        let outcome = self.prepare_and_send(&c, sequence, notification).await;
        let attempts = attempts + 1;
        let t = now();
        match outcome {
            Ok(_) => {
                c.execute(
                    "UPDATE notification_deliveries SET state='delivered',attempts=?,completed_at=?,lease_until=0,last_error=NULL WHERE id=?",
                    params![attempts, t, id],
                )
                .await?;
                eprintln!("event=notification_delivered delivery={id} attempts={attempts}");
            }
            Err(e) if e.retryable && attempts < MAX_ATTEMPTS => {
                let delay = backoff_seconds(attempts).max(i64::from(e.retry_after.unwrap_or(0)));
                c.execute(
                    "UPDATE notification_deliveries SET attempts=?,next_attempt_at=?,lease_until=0,last_error=? WHERE id=?",
                    params![attempts, t + delay, e.code, id],
                )
                .await?;
                eprintln!(
                    "event=notification_retry delivery={id} attempts={attempts} class={}",
                    e.code
                );
            }
            Err(e) => {
                c.execute(
                    "UPDATE notification_deliveries SET state='failed',attempts=?,completed_at=?,lease_until=0,last_error=? WHERE id=?",
                    params![attempts, t, e.code, id],
                )
                .await?;
                eprintln!(
                    "event=notification_failed delivery={id} attempts={attempts} class={}",
                    e.code
                );
            }
        }
        Ok(())
    }

    async fn prepare_and_send(
        &self,
        c: &Connection,
        sequence: i64,
        notification: &str,
    ) -> Result<bool, DeliveryError> {
        let storage = || DeliveryError {
            code: "storage",
            retryable: true,
            retry_after: None,
        };
        let rt = match self.store.runtime(c, notification).await {
            Ok(Some(rt)) => rt,
            Ok(None) => return Err(permanent("notification_removed")),
            Err(e) if e.status.is_server_error() && e.code == "notification_storage_error" => {
                return Err(storage());
            }
            Err(_) => return Err(permanent("credentials_unavailable")),
        };
        if !rt.notification.enabled {
            return Err(permanent("notification_disabled"));
        }
        let mut rows = c
            .query(
                "SELECT kind,scope,subject_id,payload_json,tag_ids_json FROM notification_events WHERE sequence=?",
                [sequence],
            )
            .await
            .map_err(|_| storage())?;
        let r = rows
            .next()
            .await
            .map_err(|_| storage())?
            .ok_or_else(|| permanent("event_discarded"))?;
        let event = EventRow {
            sequence,
            kind: EventKind::parse(&r.get::<String>(0).map_err(|_| storage())?)
                .ok_or_else(|| permanent("event_invalid"))?,
            scope: Scope::parse(&r.get::<String>(1).map_err(|_| storage())?)
                .ok_or_else(|| permanent("event_invalid"))?,
            subject_id: r.get(2).map_err(|_| storage())?,
            payload: serde_json::from_str(&r.get::<String>(3).map_err(|_| storage())?)
                .map_err(|_| permanent("event_invalid"))?,
            tag_ids: None,
        };
        let domain = match event.scope {
            Scope::Domain(d) => d,
            Scope::System => matched_scope_domain(c, &event, notification)
                .await
                .map_err(|_| storage())?
                .unwrap_or_else(|| {
                    if rt.notification.tv.is_some() {
                        MediaDomain::Tv
                    } else {
                        MediaDomain::Movies
                    }
                }),
        };
        let message = events::build(c, &event, domain)
            .await
            .map_err(|_| storage())?;
        self.send(&rt, &message).await
    }

    /// One poll cycle.
    pub async fn tick(&self) {
        if let Err(e) = self.fan_out().await {
            eprintln!(
                "event=notification_fan_out_error kind={:?}",
                std::mem::discriminant(&e)
            );
        }
        if let Err(e) = self.deliver_due().await {
            eprintln!(
                "event=notification_deliver_error kind={:?}",
                std::mem::discriminant(&e)
            );
        }
    }
}

async fn catalog_plex(
    transport: &Transport,
    config: &Config<'_>,
    msg: &Message,
) -> Result<bool, DeliveryError> {
    let n = catalog::plex(transport, config, msg).await?;
    Ok(n > 0)
}

fn trigger_flag(kind: EventKind) -> Option<&'static str> {
    Some(match kind {
        EventKind::Grab => "on_grab",
        EventKind::Import => "on_import",
        EventKind::Rename => "on_rename",
        EventKind::Add => "on_add",
        EventKind::Delete => "on_delete",
        EventKind::FileDelete => "on_file_delete",
        EventKind::HealthIssue => "on_health_issue",
        EventKind::HealthRestored => "on_health_restored",
        EventKind::ApplicationUpdate => "on_application_update",
        EventKind::ManualInteraction => "on_manual_interaction",
        EventKind::Test => return None,
    })
}

#[derive(Debug)]
struct ScopeCandidate {
    id: String,
    media: String,
    on_upgrade: bool,
    on_file_delete_for_upgrade: bool,
    include_health_warnings: bool,
}

#[derive(Clone, Copy)]
struct EventFilters {
    upgrade: bool,
    delete_for_upgrade: bool,
    warning_only: bool,
}

impl EventFilters {
    fn from(event: &EventRow) -> Self {
        Self {
            upgrade: event.payload.get("is_upgrade").and_then(Value::as_i64) == Some(1),
            delete_for_upgrade: event.payload.get("reason").and_then(Value::as_str)
                == Some("Upgrade"),
            warning_only: event.kind == EventKind::HealthIssue
                && event
                    .payload
                    .get("severity")
                    .and_then(Value::as_i64)
                    .unwrap_or(3)
                    < 3,
        }
    }
}

async fn live_tags(
    c: &Connection,
    event: &EventRow,
) -> Result<Option<BTreeSet<i64>>, libsql::Error> {
    match (event.tag_ids.as_ref(), event.scope, event.subject_id) {
        (Some(tags), _, _) => Ok(Some(tags.iter().copied().collect())),
        (None, Scope::Domain(domain), Some(subject)) => {
            let sql = match domain {
                MediaDomain::Tv => "SELECT tag_id FROM series_tags WHERE series_id=?",
                MediaDomain::Movies => "SELECT tag_id FROM movie_tags WHERE movie_id=?",
            };
            let mut set = BTreeSet::new();
            let mut rows = c.query(sql, [subject]).await?;
            while let Some(r) = rows.next().await? {
                set.insert(r.get::<i64>(0)?);
            }
            Ok(Some(set))
        }
        _ => Ok(None),
    }
}

async fn scope_candidates(
    c: &Connection,
    event: &EventRow,
    notification: Option<&str>,
) -> Result<Vec<ScopeCandidate>, libsql::Error> {
    let Some(flag) = trigger_flag(event.kind) else {
        return Ok(Vec::new());
    };
    let filter = if notification.is_some() {
        " AND n.id=?2"
    } else {
        ""
    };
    let sql = format!(
        "SELECT n.id,s.media_type,s.on_upgrade,s.on_file_delete_for_upgrade,s.include_health_warnings \
         FROM notifications n JOIN notification_scopes s ON s.notification_id=n.id \
         WHERE n.enabled=1 AND (?1='system' OR s.media_type=?1) AND s.{flag}=1{filter} \
         ORDER BY n.id,s.media_type DESC"
    );
    let mut rows = if let Some(notification) = notification {
        c.query(&sql, params![event.scope.as_str(), notification.to_owned()])
            .await?
    } else {
        c.query(&sql, [event.scope.as_str()]).await?
    };
    let mut candidates = Vec::new();
    while let Some(r) = rows.next().await? {
        candidates.push(ScopeCandidate {
            id: r.get(0)?,
            media: r.get(1)?,
            on_upgrade: r.get::<i64>(2)? == 1,
            on_file_delete_for_upgrade: r.get::<i64>(3)? == 1,
            include_health_warnings: r.get::<i64>(4)? == 1,
        });
    }
    Ok(candidates)
}

async fn accepts_scope(
    c: &Connection,
    event: &EventRow,
    filters: EventFilters,
    live_tags: &Option<BTreeSet<i64>>,
    candidate: &ScopeCandidate,
) -> Result<bool, libsql::Error> {
    if filters.upgrade && !candidate.on_upgrade {
        return Ok(false);
    }
    if filters.delete_for_upgrade
        && event.kind == EventKind::FileDelete
        && !candidate.on_file_delete_for_upgrade
    {
        return Ok(false);
    }
    if filters.warning_only && !candidate.include_health_warnings {
        return Ok(false);
    }
    if let (Some(tags), Some(_)) = (live_tags, event.subject_id) {
        let mut rows = c
            .query(
                "SELECT tag_id FROM notification_tags WHERE notification_id=? AND media_type=?",
                params![candidate.id.clone(), candidate.media.clone()],
            )
            .await?;
        let mut wanted = BTreeSet::new();
        while let Some(r) = rows.next().await? {
            wanted.insert(r.get::<i64>(0)?);
        }
        if !wanted.is_empty() && wanted.is_disjoint(tags) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn matching_scopes(
    c: &Connection,
    event: &EventRow,
    notification: Option<&str>,
) -> Result<Vec<(String, String)>, libsql::Error> {
    let filters = EventFilters::from(event);
    let tags = live_tags(c, event).await?;
    let mut matches = Vec::new();
    for candidate in scope_candidates(c, event, notification).await? {
        if accepts_scope(c, event, filters, &tags, &candidate).await? {
            matches.push((candidate.id, candidate.media));
        }
    }
    Ok(matches)
}

/// Notification ids that should receive this event: enabled, scoped to the event domain, trigger
/// flag set, tag scope satisfied. One id per notification even when several scopes match.
pub async fn matching(c: &Connection, event: &EventRow) -> Result<Vec<String>, libsql::Error> {
    let mut out = Vec::new();
    for (id, _) in matching_scopes(c, event, None).await? {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

async fn matched_scope_domain(
    c: &Connection,
    event: &EventRow,
    notification: &str,
) -> Result<Option<MediaDomain>, libsql::Error> {
    let Some((_, media)) = matching_scopes(c, event, Some(notification))
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    Ok(match media.as_str() {
        "tv" => Some(MediaDomain::Tv),
        "movies" => Some(MediaDomain::Movies),
        _ => None,
    })
}

pub struct Handle {
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Handle {
    pub async fn shutdown(self) {
        self.stop.notify_waiters();
        self.stop.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

/// Starts the polling worker. Call only where this process owns the database.
pub fn start(dispatcher: Dispatcher) -> Handle {
    let stop = Arc::new(Notify::new());
    let signal = stop.clone();
    let task = tokio::spawn(async move {
        loop {
            dispatcher.tick().await;
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = signal.notified() => break,
            }
        }
    });
    Handle { stop, task }
}
