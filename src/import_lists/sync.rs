//! Bounded, resumable list synchronisation.
//!
//! A run is a durable row (`import_list_runs`). Claiming sets a lease; a process that dies mid-run
//! leaves a `running` row whose lease expires, and the next claim resumes it: the fetched snapshot
//! (`import_list_items`, capped at 5000 rows per list) and every item's state survive, so the
//! network fetch is not repeated and finished items are not redone. Each claim processes at most
//! [`PASS_ITEMS`] items and re-queues the run if more remain, so one large list never monopolises
//! the worker and memory stays bounded by the snapshot cap. No database transaction is held across
//! a network call: fetches and metadata lookups complete first, then short writes follow.
use super::{
    catalog,
    error::{ApiError, Result},
    model::*,
    providers::FetchError,
    store::{Store, domain_name, now},
};
use crate::{
    api::MediaDomain,
    library::{self, AddFailure, Change, ListAdd, NewItems, Patch, SeasonMonitor},
    metadata::MetadataClient,
    notifications::transport::Transport,
    search::parser::normalize,
};
use libsql::{Connection, TransactionBehavior, params};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

pub const TICK: Duration = Duration::from_secs(5);
const LEASE_SECONDS: i64 = 180;
/// Items added or settled per claim; the run is re-queued while more remain.
pub const PASS_ITEMS: i64 = 25;
const MAX_ATTEMPTS: i64 = 3;
const BACKOFF_BASE_SECONDS: i64 = 60;
const FAILED_RETRY_SECONDS: i64 = 3600;
const UNRESOLVED_RETRY_SECONDS: i64 = 86_400;
const FAILURE_RESYNC_CAP_SECONDS: i64 = 1800;

#[derive(Clone)]
pub struct Engine {
    pub store: Arc<Store>,
    pub transport: Arc<Transport>,
    pub metadata: Arc<MetadataClient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Added(i64),
    Existing(i64),
    Excluded,
    Unresolved,
    Failed(&'static str),
}

struct PendingItem {
    key: String,
    tvdb_id: Option<i64>,
    tmdb_id: Option<i64>,
    imdb_id: Option<String>,
    title: String,
    year: Option<i64>,
}

pub fn backoff_seconds(attempts: i64) -> i64 {
    let exp = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(6);
    BACKOFF_BASE_SECONDS
        .saturating_mul(4i64.saturating_pow(exp))
        .min(6 * 3600)
}

impl Engine {
    pub fn new(
        store: Arc<Store>,
        transport: Arc<Transport>,
        metadata: Arc<MetadataClient>,
    ) -> Self {
        Self {
            store,
            transport,
            metadata,
        }
    }

    /// Queues a sync of one list. An already active run is returned as is (`false`).
    pub async fn enqueue(&self, list_id: &str, origin: &str) -> Result<(Run, bool)> {
        let id = Uuid::parse_str(list_id)
            .map_err(|_| ApiError::invalid("Import list ID must be a UUID"))?
            .to_string();
        let c = self.store.db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let (media, enabled) = {
            let mut rows = tx
                .query(
                    "SELECT media_type,enabled FROM import_lists WHERE id=?",
                    [id.clone()],
                )
                .await?;
            let row = rows.next().await?.ok_or_else(ApiError::not_found)?;
            (row.get::<String>(0)?, row.get::<i64>(1)? == 1)
        };
        if !enabled {
            return Err(ApiError::conflict(
                "import_list_disabled",
                "Enable the import list before syncing it",
            ));
        }
        if let Some(active) = active_run(&tx, &id).await? {
            let run = read_run(&tx, &active)
                .await?
                .ok_or_else(ApiError::not_found)?;
            tx.commit().await?;
            return Ok((run, false));
        }
        let run_id = Uuid::new_v4().to_string();
        let t = now();
        tx.execute(
            "INSERT INTO import_list_runs(id,list_id,media_type,origin,status,next_attempt_at,created_at) VALUES(?,?,?,?,'queued',?,?)",
            params![run_id.clone(), id, media, origin, t, t],
        )
        .await?;
        let run = read_run(&tx, &run_id)
            .await?
            .ok_or_else(ApiError::not_found)?;
        tx.commit().await?;
        Ok((run, true))
    }

    /// Queues every enabled list that has no active run.
    pub async fn enqueue_all(&self, origin: &str) -> Result<SyncAll> {
        let c = self.store.db.connect().await?;
        let mut ids = Vec::new();
        let mut skipped = 0;
        let mut rows = c
            .query(
                "SELECT id,enabled FROM import_lists ORDER BY created_at,id",
                (),
            )
            .await?;
        while let Some(r) = rows.next().await? {
            if r.get::<i64>(1)? == 1 {
                ids.push(r.get::<String>(0)?);
            } else {
                skipped += 1;
            }
        }
        drop(rows);
        let mut runs = Vec::new();
        for id in ids {
            match self.enqueue(&id, origin).await {
                Ok((run, true)) => runs.push(run),
                Ok((_, false)) => skipped += 1,
                Err(e)
                    if matches!(
                        e.status,
                        axum::http::StatusCode::NOT_FOUND | axum::http::StatusCode::CONFLICT
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(SyncAll { runs, skipped })
    }

    /// Queues scheduled runs for lists whose interval has elapsed.
    pub async fn schedule_due(&self) -> Result<usize> {
        let c = self.store.db.connect().await?;
        let t = now();
        let mut due = Vec::new();
        let mut rows = c
            .query(
                &format!(
                    "SELECT id FROM import_lists l WHERE enabled=1 AND NOT EXISTS(SELECT 1 FROM import_list_runs r WHERE r.list_id=l.id AND r.status IN ('queued','running')) AND coalesce(last_sync_at,0)+(CASE WHEN last_error IS NULL THEN interval_minutes*60 ELSE min(interval_minutes*60,{FAILURE_RESYNC_CAP_SECONDS}) END)<=? ORDER BY coalesce(last_sync_at,0),id LIMIT 16"
                ),
                [t],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            due.push(r.get::<String>(0)?);
        }
        drop(rows);
        let mut queued = 0;
        for id in due {
            if let Ok((_, true)) = self.enqueue(&id, "scheduled").await {
                queued += 1;
            }
        }
        Ok(queued)
    }

    /// Claims one runnable run: queued and due, or running with an expired lease (a crashed claim).
    async fn claim(&self) -> Result<Option<(String, String, bool)>> {
        let c = self.store.db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let t = now();
        let found = {
            let mut rows = tx
                .query(
                    "SELECT id,list_id,fetched_at FROM import_list_runs WHERE (status='queued' AND next_attempt_at<=?1) OR (status='running' AND lease_until<?1) ORDER BY created_at,rowid LIMIT 1",
                    [t],
                )
                .await?;
            match rows.next().await? {
                Some(r) => Some((
                    r.get::<String>(0)?,
                    r.get::<String>(1)?,
                    r.get::<Option<i64>>(2)?.is_some(),
                )),
                None => None,
            }
        };
        if let Some((id, _, _)) = &found {
            tx.execute(
                "UPDATE import_list_runs SET status='running',lease_until=?,started_at=coalesce(started_at,?) WHERE id=?",
                params![t + LEASE_SECONDS, t, id.clone()],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(found)
    }

    /// One scheduler step: queue due lists, then run at most `limit` claimed passes.
    pub async fn tick(&self, limit: usize) -> Result<usize> {
        self.schedule_due().await?;
        let mut done = 0;
        while done < limit {
            let Some((run_id, list_id, fetched)) = self.claim().await? else {
                break;
            };
            self.process(&run_id, &list_id, fetched).await?;
            done += 1;
        }
        Ok(done)
    }

    /// Runs claimed passes until no run is runnable (bounded), for tests and `sync now` callers.
    pub async fn drain(&self) -> Result<usize> {
        let mut total = 0;
        for _ in 0..256 {
            let n = self.tick(8).await?;
            total += n;
            if n == 0 {
                break;
            }
        }
        Ok(total)
    }

    async fn process(&self, run_id: &str, list_id: &str, fetched: bool) -> Result<()> {
        let c = self.store.db.connect().await?;
        let loaded = match self.store.runtime(&c, list_id).await {
            Ok(Some(loaded)) => loaded,
            Ok(None) => return Ok(()),
            Err(e) => {
                // Locked credentials cannot succeed by retrying.
                let code = if e.code == "import_list_key_unavailable" {
                    "key_unavailable"
                } else {
                    "credentials_locked"
                };
                return finish(&c, run_id, list_id, Err(code)).await;
            }
        };
        let (list, runtime) = loaded;
        if !fetched {
            if !list.enabled {
                return finish(&c, run_id, list_id, Ok(())).await;
            }
            match catalog::fetch(&self.transport, &runtime).await {
                Ok(items) => persist_snapshot(&c, run_id, &list, &items).await?,
                Err(error) => return fail_or_retry(&c, run_id, list_id, error).await,
            }
        }
        if !(list.enabled && list.enable_automatic_add) {
            return finish(&c, run_id, list_id, Ok(())).await;
        }
        let Some(target) = AddTarget::load(&c, &list).await? else {
            return finish(&c, run_id, list_id, Err("not_configured")).await;
        };
        let pending = pending_items(&c, list_id, PASS_ITEMS).await?;
        for item in pending {
            // Keep the lease alive per item; a long pass cannot be reclaimed from under itself.
            c.execute(
                "UPDATE import_list_runs SET lease_until=? WHERE id=?",
                params![now() + LEASE_SECONDS, run_id],
            )
            .await?;
            let outcome = self.process_item(&list, &target, run_id, &item).await;
            settle(&c, run_id, list_id, &item.key, &outcome).await?;
        }
        if count_pending(&c, list_id).await? > 0 {
            // Resume on the next claim: the snapshot and item states are already durable.
            c.execute(
                "UPDATE import_list_runs SET status='queued',lease_until=0,next_attempt_at=? WHERE id=?",
                params![now(), run_id],
            )
            .await?;
            return Ok(());
        }
        finish(&c, run_id, list_id, Ok(())).await
    }

    async fn process_item(
        &self,
        list: &ImportList,
        target: &AddTarget,
        run_id: &str,
        item: &PendingItem,
    ) -> Outcome {
        let media = list.media_type;
        // 1. Library identity: the list's own id, else exact-match metadata resolution.
        let external = match media {
            MediaDomain::Tv => item.tvdb_id,
            MediaDomain::Movies => item.tmdb_id,
        };
        let external = match external {
            Some(id) => id,
            None => match self.resolve(media, item).await {
                Ok(Some(id)) => id,
                Ok(None) => return Outcome::Unresolved,
                Err(code) => return Outcome::Failed(code),
            },
        };
        let Ok(c) = self.store.db.connect().await else {
            return Outcome::Failed("storage_unavailable");
        };
        // 2. Exclusions and existing membership are cheap local checks.
        match super::excluded(&c, media, external).await {
            Ok(true) => return Outcome::Excluded,
            Ok(false) => {}
            Err(_) => return Outcome::Failed("storage_unavailable"),
        }
        match existing_library_id(&c, media, external).await {
            Ok(Some(id)) => return Outcome::Existing(id),
            Ok(None) => {}
            Err(_) => return Outcome::Failed("storage_unavailable"),
        }
        // 3. The one safe add path, with provenance and the exclusion fence inside its transaction.
        let search = list.search_on_add && list.monitor != Monitor::None;
        let add = ListAdd {
            list_id: list.id.to_string(),
            list_name: list.name.clone(),
            run_id: run_id.to_owned(),
            season_monitor: match list.monitor {
                Monitor::All => SeasonMonitor::All,
                Monitor::None => SeasonMonitor::None,
                Monitor::FirstSeason => SeasonMonitor::FirstSeason,
                Monitor::LastSeason => SeasonMonitor::LastSeason,
            },
            search_requested: search,
        };
        let path = format!(
            "{}/{}",
            target.root_path,
            folder_name(&item.title, item.year)
        );
        match library::add_from_catalog(
            &self.store.db,
            &self.metadata,
            media,
            external,
            path,
            target.patch(list),
            Some(&add),
        )
        .await
        {
            Ok(created) => Outcome::Added(created.id),
            Err(AddFailure::Excluded) => Outcome::Excluded,
            Err(AddFailure::Response(response)) => match response.status().as_u16() {
                404 => Outcome::Unresolved,
                409 => Outcome::Failed("library_conflict"),
                502 | 503 | 504 => Outcome::Failed("metadata_unavailable"),
                _ => Outcome::Failed("add_rejected"),
            },
        }
    }

    /// Exact normalised-title (and, when both sides know it, year) match against the metadata
    /// service; ambiguity is left unresolved rather than guessed.
    async fn resolve(
        &self,
        media: MediaDomain,
        item: &PendingItem,
    ) -> std::result::Result<Option<i64>, &'static str> {
        let unavailable = |_| "metadata_unavailable";
        let results = match media {
            MediaDomain::Movies => {
                if let Some(imdb) = &item.imdb_id {
                    let by_id = self
                        .metadata
                        .lookup_movie(&format!("imdb:{imdb}"))
                        .await
                        .map_err(unavailable)?;
                    if by_id.len() == 1 {
                        return Ok(Some(by_id[0].external_id));
                    }
                }
                self.metadata
                    .lookup_movie(&item.title)
                    .await
                    .map_err(unavailable)?
            }
            MediaDomain::Tv => self
                .metadata
                .lookup_tv(&item.title)
                .await
                .map_err(unavailable)?,
        };
        let wanted = normalize(&item.title);
        let mut matches: Vec<&crate::metadata::LookupResult> = results
            .iter()
            .filter(|r| normalize(&r.title) == wanted)
            .filter(|r| match (item.year, r.year) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            })
            .collect();
        matches.dedup_by_key(|r| r.external_id);
        Ok(match matches.as_slice() {
            [only] => Some(only.external_id),
            _ => None,
        })
    }
}

/// Where and how this list's items are added.
struct AddTarget {
    root_path: String,
    quality_profile_id: i64,
}
impl AddTarget {
    async fn load(c: &Connection, list: &ImportList) -> Result<Option<Self>> {
        let (Some(root), Some(profile)) = (list.root_folder_id, list.quality_profile_id) else {
            return Ok(None);
        };
        let mut rows = c
            .query(
                "SELECT path FROM root_folders WHERE id=? AND media_type=?",
                params![root, domain_name(list.media_type)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(Self {
            root_path: row.get(0)?,
            quality_profile_id: profile,
        }))
    }
    fn patch(&self, list: &ImportList) -> Patch {
        let tags = (!list.tag_ids.is_empty()).then(|| crate::tags::Assignment {
            mode: crate::tags::Mode::Replace,
            ids: list.tag_ids.clone(),
        });
        let mut patch = Patch {
            quality_profile_id: Change::Value(self.quality_profile_id),
            monitored: Change::Value(list.monitor != Monitor::None),
            ..Patch::default()
        };
        if let Some(tags) = tags {
            patch.tags = Change::Value(tags);
        }
        if list.media_type == MediaDomain::Tv {
            if let Some(kind) = list.series_type.clone() {
                patch.series_type = Change::Value(kind);
            }
            if let Some(folder) = list.season_folder {
                patch.season_folder = Change::Value(folder);
            }
            patch.monitor_new_items = Change::Value(NewItems::All);
        } else if let Some(availability) = list.minimum_availability.clone() {
            patch.minimum_availability = Change::Value(availability);
        }
        patch
    }
}

/// `Title (Year)` made safe as one path segment: separators, control characters and leading dots
/// are neutralised and the segment is bounded to 200 bytes.
pub fn folder_name(title: &str, year: Option<i64>) -> String {
    let mut name: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    name = name
        .trim_matches(|c: char| c == '.' || c == ' ' || c == '-')
        .to_owned();
    if name.is_empty() {
        name = "untitled".into();
    }
    while name.len() > 190 {
        name.pop();
    }
    match year {
        Some(y) => format!("{name} ({y})"),
        None => name,
    }
}

async fn existing_library_id(
    c: &Connection,
    media: MediaDomain,
    external: i64,
) -> Result<Option<i64>> {
    let sql = match media {
        MediaDomain::Tv => "SELECT id FROM series WHERE tvdb_id=?",
        MediaDomain::Movies => {
            "SELECT m.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE d.tmdb_id=?"
        }
    };
    let mut rows = c.query(sql, [external]).await?;
    Ok(match rows.next().await? {
        Some(r) => Some(r.get::<i64>(0)?),
        None => None,
    })
}

async fn pending_items(c: &Connection, list_id: &str, limit: i64) -> Result<Vec<PendingItem>> {
    let mut rows = c
        .query(
            "SELECT key,tvdb_id,tmdb_id,imdb_id,title,year FROM import_list_items WHERE list_id=? AND state='pending' ORDER BY seq LIMIT ?",
            params![list_id, limit],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(PendingItem {
            key: r.get(0)?,
            tvdb_id: r.get(1)?,
            tmdb_id: r.get(2)?,
            imdb_id: r.get(3)?,
            title: r.get(4)?,
            year: r.get(5)?,
        });
    }
    Ok(out)
}
async fn count_pending(c: &Connection, list_id: &str) -> Result<i64> {
    super::store::scalar(
        c,
        "SELECT count(*) FROM import_list_items WHERE list_id=? AND state='pending'",
        [list_id.to_owned()],
    )
    .await
}

/// Replaces the list's membership with a complete fetch, atomically. Finished items keep their
/// state; items that left the list are dropped; failed and unresolved items are retried after a
/// cool-down rather than on every run.
async fn persist_snapshot(
    c: &Connection,
    run_id: &str,
    list: &ImportList,
    items: &[ListItem],
) -> Result<()> {
    let list_id = list.id.to_string();
    let t = now();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut existing: BTreeMap<String, (String, i64)> = BTreeMap::new();
    {
        let mut rows = tx
            .query(
                "SELECT key,state,updated_at FROM import_list_items WHERE list_id=?",
                [list_id.clone()],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            existing.insert(r.get(0)?, (r.get(1)?, r.get(2)?));
        }
    }
    let keyed: Vec<(String, &ListItem)> = items
        .iter()
        .map(|i| (super::providers::item_key(list.media_type, i), i))
        .collect();
    let live: std::collections::BTreeSet<&str> = keyed.iter().map(|(k, _)| k.as_str()).collect();
    for key in existing.keys().filter(|k| !live.contains(k.as_str())) {
        tx.execute(
            "DELETE FROM import_list_items WHERE list_id=? AND key=?",
            params![list_id.clone(), key.clone()],
        )
        .await?;
    }
    for (seq, (key, item)) in keyed.iter().enumerate() {
        let seq = i64::try_from(seq).unwrap_or(0);
        match existing.get(key) {
            Some((state, updated)) => {
                let keep = match state.as_str() {
                    "added" => true,
                    "failed" => t - updated < FAILED_RETRY_SECONDS,
                    "unresolved" => t - updated < UNRESOLVED_RETRY_SECONDS,
                    _ => false,
                };
                let next = if keep { state.as_str() } else { "pending" };
                tx.execute(
                    "UPDATE import_list_items SET seq=?,tvdb_id=?,tmdb_id=?,imdb_id=?,title=?,year=?,state=?,error_code=CASE WHEN ?='pending' THEN NULL ELSE error_code END,updated_at=CASE WHEN ?=state THEN updated_at ELSE ? END WHERE list_id=? AND key=?",
                    params![seq, item.tvdb_id, item.tmdb_id, item.imdb_id.clone(), item.title.clone(), item.year, next, next, next, t, list_id.clone(), key.clone()],
                )
                .await?;
            }
            None => {
                tx.execute(
                    "INSERT INTO import_list_items(list_id,key,seq,tvdb_id,tmdb_id,imdb_id,title,year,state,updated_at) VALUES(?,?,?,?,?,?,?,?,'pending',?)",
                    params![list_id.clone(), key.clone(), seq, item.tvdb_id, item.tmdb_id, item.imdb_id.clone(), item.title.clone(), item.year, t],
                )
                .await?;
            }
        }
    }
    tx.execute(
        "UPDATE import_list_runs SET fetched_at=?,items_fetched=? WHERE id=?",
        params![t, i64::try_from(keyed.len()).unwrap_or(0), run_id],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn settle(
    c: &Connection,
    run_id: &str,
    list_id: &str,
    key: &str,
    outcome: &Outcome,
) -> Result<()> {
    let (state, error, library, counter) = match outcome {
        Outcome::Added(id) => ("added", None, Some(*id), "added"),
        Outcome::Existing(id) => ("existing", None, Some(*id), "existing"),
        Outcome::Excluded => ("excluded", None, None, "excluded"),
        Outcome::Unresolved => ("unresolved", None, None, "unresolved"),
        Outcome::Failed(code) => ("failed", Some(*code), None, "failed"),
    };
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "UPDATE import_list_items SET state=?,error_code=?,library_id=?,updated_at=? WHERE list_id=? AND key=?",
        params![state, error, library, now(), list_id, key],
    )
    .await?;
    tx.execute(
        &format!("UPDATE import_list_runs SET {counter}={counter}+1 WHERE id=?"),
        [run_id],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn finish(
    c: &Connection,
    run_id: &str,
    list_id: &str,
    result: std::result::Result<(), &'static str>,
) -> Result<()> {
    let t = now();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    match result {
        Ok(()) => {
            tx.execute(
                "UPDATE import_list_runs SET status='succeeded',completed_at=?,lease_until=0,error_code=NULL WHERE id=?",
                params![t, run_id],
            )
            .await?;
            tx.execute(
                "UPDATE import_lists SET last_sync_at=?,last_success_at=?,last_error=NULL WHERE id=?",
                params![t, t, list_id],
            )
            .await?;
        }
        Err(code) => {
            tx.execute(
                "UPDATE import_list_runs SET status='failed',completed_at=?,lease_until=0,error_code=? WHERE id=?",
                params![t, code, run_id],
            )
            .await?;
            tx.execute(
                "UPDATE import_lists SET last_sync_at=?,last_error=? WHERE id=?",
                params![t, code, list_id],
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn fail_or_retry(
    c: &Connection,
    run_id: &str,
    list_id: &str,
    error: FetchError,
) -> Result<()> {
    let attempts = super::store::scalar(
        c,
        "SELECT attempts FROM import_list_runs WHERE id=?",
        [run_id.to_owned()],
    )
    .await?
        + 1;
    if error.retryable() && attempts < MAX_ATTEMPTS {
        c.execute(
            "UPDATE import_list_runs SET status='queued',lease_until=0,attempts=?,next_attempt_at=?,error_code=? WHERE id=?",
            params![attempts, now() + backoff_seconds(attempts), error.code(), run_id],
        )
        .await?;
        return Ok(());
    }
    c.execute(
        "UPDATE import_list_runs SET attempts=? WHERE id=?",
        params![attempts, run_id],
    )
    .await?;
    finish(c, run_id, list_id, Err(error.code())).await
}

async fn active_run(c: &Connection, list_id: &str) -> Result<Option<String>> {
    let mut rows = c
        .query(
            "SELECT id FROM import_list_runs WHERE list_id=? AND status IN ('queued','running')",
            [list_id.to_owned()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(r) => Some(r.get::<String>(0)?),
        None => None,
    })
}

const RUN_COLUMNS: &str = "r.id,r.list_id,r.media_type,r.origin,r.status,r.fetched_at,r.attempts,r.items_fetched,r.added,r.existing,r.excluded,r.unresolved,r.failed,(SELECT count(*) FROM import_list_items i WHERE i.list_id=r.list_id AND i.state='pending'),r.error_code,r.created_at,r.started_at,r.completed_at";

fn run_from(r: &libsql::Row) -> Result<Run> {
    Ok(Run {
        id: Uuid::parse_str(&r.get::<String>(0)?).unwrap_or_default(),
        list_id: Uuid::parse_str(&r.get::<String>(1)?).unwrap_or_default(),
        media_type: MediaDomain::parse(&r.get::<String>(2)?).map_err(|_| ApiError::not_found())?,
        origin: r.get(3)?,
        status: RunStatus::parse(&r.get::<String>(4)?),
        fetched: r.get::<Option<i64>>(5)?.is_some(),
        attempts: r.get(6)?,
        items_fetched: r.get(7)?,
        added: r.get(8)?,
        existing: r.get(9)?,
        excluded: r.get(10)?,
        unresolved: r.get(11)?,
        failed: r.get(12)?,
        pending: r.get(13)?,
        error_code: r.get(14)?,
        created_at: r.get(15)?,
        started_at: r.get(16)?,
        completed_at: r.get(17)?,
    })
}
pub async fn read_run(c: &Connection, run_id: &str) -> Result<Option<Run>> {
    let mut rows = c
        .query(
            &format!("SELECT {RUN_COLUMNS} FROM import_list_runs r WHERE r.id=?"),
            [run_id.to_owned()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(r) => Some(run_from(&r)?),
        None => None,
    })
}
pub async fn list_runs(c: &Connection, list_id: &str, limit: i64) -> Result<Vec<Run>> {
    let mut rows = c
        .query(
            &format!("SELECT {RUN_COLUMNS} FROM import_list_runs r WHERE r.list_id=? ORDER BY r.created_at DESC,r.rowid DESC LIMIT ?"),
            params![list_id, limit],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(run_from(&r)?);
    }
    Ok(out)
}
