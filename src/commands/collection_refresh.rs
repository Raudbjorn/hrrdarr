//! RefreshCollections (job.012): refreshes every collection graph from the movie metadata service
//! and, for monitored collections with a complete policy, adds missing members to the library.
//! One active command at a time (unique index); a run may span several worker slices via a cursor.
//! Scheduled once per interval when collections exist; manual runs are always admitted.
use super::*;
use crate::{
    collections,
    metadata::{MetadataClient, MetadataError},
};

const INTERVAL_SECONDS: i64 = 24 * 60 * 60;
const SLICE_BUDGET: std::time::Duration = std::time::Duration::from_secs(25);
const SLICE_COLLECTIONS: i64 = 50;
const MAX_CONTINUATIONS: i64 = 1000;
/// Delay before retrying after a busy metadata lane (a rate limit carries its own `Retry-After`).
const BUSY_RETRY_SECONDS: i64 = 30;
const MAX_RETRY_SECONDS: i64 = 3600;
/// A run that keeps meeting transient errors gives up this long after it started.
const RETRY_WINDOW_SECONDS: i64 = 6 * 60 * 60;
const RETAINED_TERMINAL: i64 = 100;

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CollectionRefreshName {
    RefreshCollections,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CollectionRefreshOrigin {
    Manual,
    Automatic,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CollectionRefreshInput {}
#[derive(Serialize, ts_rs::TS)]
pub struct CollectionRefreshCommand {
    pub id: Uuid,
    pub name: CollectionRefreshName,
    pub origin: CollectionRefreshOrigin,
    pub status: CommandStatus,
    pub attempts: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub collections_checked: i64,
    pub collections_failed: i64,
    pub movies_added: i64,
    /// Added movies whose collection requested search-on-add. Counted, not submitted here.
    pub searches_pending: i64,
    #[serde(skip)]
    #[ts(skip)]
    cursor: i64,
}
const COLUMNS: &str = "id,origin,status,attempts,created_at,started_at,completed_at,error_code,collections_checked,collections_failed,movies_added,searches_pending,cursor";
fn row(r: libsql::Row) -> Result<CollectionRefreshCommand> {
    Ok(CollectionRefreshCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: CollectionRefreshName::RefreshCollections,
        origin: match r.get::<String>(1)?.as_str() {
            "manual" => CollectionRefreshOrigin::Manual,
            "automatic" => CollectionRefreshOrigin::Automatic,
            _ => return Err(bad()),
        },
        status: CommandStatus::parse(&r.get::<String>(2)?)?,
        attempts: r.get(3)?,
        created_at: r.get(4)?,
        started_at: r.get(5)?,
        completed_at: r.get(6)?,
        error_code: r.get(7)?,
        collections_checked: r.get(8)?,
        collections_failed: r.get(9)?,
        movies_added: r.get(10)?,
        searches_pending: r.get(11)?,
        cursor: r.get(12)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<CollectionRefreshCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM collection_refresh_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::NOT_FOUND,
            "collection_refresh_command_not_found",
        ))?)
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route(
            "/api/v1/movies/collections/refresh-commands",
            get(list).post(create),
        )
        .route(
            "/api/v1/movies/collections/refresh-commands/{id}",
            get(detail),
        )
        .layer(DefaultBodyLimit::max(1024))
        .with_state(db)
}
async fn enqueue(c: &Connection, origin: &str, timestamp: i64) -> Result<CollectionRefreshCommand> {
    if let Some(r) = c
        .query(
            &format!("SELECT {COLUMNS} FROM collection_refresh_commands WHERE status IN ('queued','running','retry_wait')"),
            (),
        )
        .await?
        .next()
        .await?
    {
        return row(r);
    }
    let id = Uuid::new_v4();
    c.execute(
        "INSERT INTO collection_refresh_commands(id,name,origin,status,next_attempt_at,created_at)VALUES(?,'refresh_collections',?,'queued',?,?)",
        params![id.to_string(), origin, timestamp, timestamp],
    )
    .await?;
    read(c, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    input: std::result::Result<Json<CollectionRefreshInput>, JsonRejection>,
) -> Result<(StatusCode, Json<CollectionRefreshCommand>)> {
    input.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async { bounded(enqueue(&tx, "manual", now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ApiPage<CollectionRefreshCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let total = c
        .query("SELECT count(*) FROM collection_refresh_commands", ())
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get(0)?;
    let mut rows = c
        .query(
            &format!("SELECT {COLUMNS} FROM collection_refresh_commands ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),
            params![i64::from(q.limit), i64::from(q.offset)],
        )
        .await?;
    let mut items = vec![];
    while let Some(r) = rows.next().await? {
        items.push(row(r)?);
    }
    bounded(ApiPage {
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CollectionRefreshQuery", optional_fields)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
) -> Result<Json<CollectionRefreshCommand>> {
    bounded(
        read(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
        )
        .await?,
    )
}

/// Inside the worker's claim transaction: one automatic run per interval while collections exist.
pub(super) async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    let due = c
        .query(
            "SELECT EXISTS(SELECT 1 FROM movie_collections WHERE tmdb_id IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM collection_refresh_commands WHERE status IN ('queued','running','retry_wait') OR created_at>?)",
            [timestamp - INTERVAL_SECONDS],
        )
        .await?
        .next()
        .await?
        .ok_or_else(bad)?
        .get::<i64>(0)?;
    if due == 1 {
        enqueue(c, "automatic", timestamp).await?;
    }
    Ok(())
}
/// Claims the queued refresh the worker selected through the shared priority ordering.
pub(super) async fn claim(
    c: &Connection,
    id: Uuid,
    timestamp: i64,
) -> Result<CollectionRefreshCommand> {
    c.execute(
        "UPDATE collection_refresh_commands SET status='running',attempts=attempts+1,started_at=COALESCE(started_at,?),error_code=NULL WHERE id=? AND status='queued'",
        params![timestamp, id.to_string()],
    )
    .await?;
    read(c, id).await
}
/// An interrupted slice resumes from its cursor; completed collections are not repeated.
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let _ = code;
    c.execute(
        "UPDATE collection_refresh_commands SET status=CASE WHEN attempts<? THEN 'queued' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<? THEN NULL ELSE ? END,error_code=CASE WHEN attempts<? THEN NULL ELSE 'interrupted' END WHERE status='running'",
        params![MAX_CONTINUATIONS, timestamp, MAX_CONTINUATIONS, timestamp, MAX_CONTINUATIONS],
    )
    .await?;
    Ok(())
}
fn map(e: collections::Error) -> Error {
    Error(e.0, e.1)
}
async fn still_running(c: &Connection, id: Uuid) -> Result<bool> {
    Ok(read(c, id)
        .await
        .is_ok_and(|v| matches!(v.status, CommandStatus::Running)))
}
pub(super) async fn run(
    db: &Arc<Database>,
    client: &MetadataClient,
    command: CollectionRefreshCommand,
) -> Result<()> {
    let id = command.id;
    let started = std::time::Instant::now();
    let c = connection(db).await?;
    let batch = collections::refreshable(&c, command.cursor, SLICE_COLLECTIONS)
        .await
        .map_err(map)?;
    let mut cursor = command.cursor;
    let (mut checked, mut failed, mut unavailable, mut added, mut searches) =
        (0i64, 0i64, 0i64, 0i64, 0i64);
    // Set when a rate limit or busy lane stops the slice: the collection that met it is not
    // settled (the cursor stays before it) and the command is requeued instead of burning through
    // the catalog.
    let mut retry_after: Option<i64> = None;
    for (collection_id, tmdb_id) in &batch {
        if started.elapsed() > SLICE_BUDGET || !still_running(&c, id).await? {
            break;
        }
        match client.collection(*tmdb_id).await {
            Ok(graph) => {
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .await?;
                match collections::apply_graph(&tx, *collection_id, &graph).await {
                    Ok((n, s)) => {
                        tx.commit().await?;
                        checked += 1;
                        added += n;
                        searches += s;
                    }
                    Err(e) => {
                        tx.rollback().await?;
                        eprintln!("event=collection_refresh_apply_error code={}", e.1);
                        failed += 1;
                    }
                }
            }
            Err(MetadataError::Unavailable) => {
                failed += 1;
                unavailable += 1;
            }
            Err(MetadataError::RateLimited(seconds)) => {
                retry_after = Some(
                    seconds
                        .map_or(BUSY_RETRY_SECONDS, i64::from)
                        .clamp(1, MAX_RETRY_SECONDS),
                );
                break;
            }
            Err(MetadataError::Busy) => {
                retry_after = Some(BUSY_RETRY_SECONDS);
                break;
            }
            Err(_) => failed += 1,
        }
        cursor = *collection_id;
    }
    let progress = "collections_checked=collections_checked+?,collections_failed=collections_failed+?,movies_added=movies_added+?,searches_pending=searches_pending+?,cursor=?";
    if let Some(delay) = retry_after {
        let timestamp = now()?;
        let gave_up = command
            .started_at
            .is_some_and(|started| timestamp - started > RETRY_WINDOW_SECONDS);
        if !gave_up {
            c.execute(
                &format!("UPDATE collection_refresh_commands SET {progress},status='queued',next_attempt_at=? WHERE id=? AND status='running'"),
                params![checked, failed, added, searches, cursor, timestamp + delay, id.to_string()],
            )
            .await?;
            eprintln!("event=collection_refresh_retry command_id={id} delay_seconds={delay}");
            return Ok(());
        }
    }
    // Still throttled after the retry window: stop with an honest failure instead of reporting a
    // run that never reached the metadata service as a success.
    let gave_up = retry_after.is_some();
    // Everything up to `cursor` is settled; anything left runs in a later slice.
    let more = !collections::refreshable(&c, cursor, 1)
        .await
        .map_err(map)?
        .is_empty();
    if more && !gave_up {
        c.execute(
            &format!("UPDATE collection_refresh_commands SET {progress},status='queued',next_attempt_at=? WHERE id=? AND status='running'"),
            params![checked, failed, added, searches, cursor, now()?, id.to_string()],
        )
        .await?;
        return Ok(());
    }
    let total_checked = command.collections_checked + checked;
    let total_failed = command.collections_failed + failed;
    let all_unavailable = gave_up || (total_checked == 0 && total_failed > 0 && unavailable > 0);
    c.execute(
        &format!("UPDATE collection_refresh_commands SET {progress},status=?,completed_at=?,error_code=? WHERE id=? AND status='running'"),
        params![
            checked,
            failed,
            added,
            searches,
            cursor,
            if all_unavailable { "failed" } else { "succeeded" },
            now()?,
            if all_unavailable { Some("metadata_unavailable") } else { None },
            id.to_string()
        ],
    )
    .await?;
    c.execute(
        "DELETE FROM collection_refresh_commands WHERE status IN ('succeeded','failed','cancelled') AND id NOT IN (SELECT id FROM collection_refresh_commands WHERE status IN ('succeeded','failed','cancelled') ORDER BY created_at DESC,id DESC LIMIT ?)",
        [RETAINED_TERMINAL],
    )
    .await?;
    eprintln!(
        "event=collection_refresh_settled command_id={id} checked={} failed={} added={}",
        total_checked + 0,
        total_failed,
        command.movies_added + added
    );
    Ok(())
}
