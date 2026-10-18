//! OnRename hook point. Every committed rename writes one immutable, domain-typed row to
//! `rename_events` in the same transaction as the path update (the durable outbox), and after
//! commit calls registered in-process observers best-effort. A notifications consumer reads
//! `pending`, delivers, then calls `mark_dispatched`; it never needs to be known here.
use crate::api::{ApiErrorEnvelope, ApiPage, MediaDomain};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

const MAX_OBSERVERS: usize = 16;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RenameEventKind {
    File,
    Folder,
}
/// Domain-typed owner: an unqualified integer never picks between a series and a movie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "media_type", content = "id", rename_all = "snake_case")]
pub enum RenameOwner {
    Tv(i64),
    Movies(i64),
}
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct RenameEvent {
    pub id: String,
    pub event_type: &'static str,
    pub kind: RenameEventKind,
    pub owner: RenameOwner,
    pub file_id: Option<i64>,
    pub command_id: String,
    pub old_path: String,
    pub new_path: String,
    pub sidecar_count: i64,
    pub occurred_at: String,
    pub dispatched: bool,
}
pub trait RenameObserver: Send + Sync {
    /// Called after the rename and its event row are committed. Must not block; failures are
    /// the observer's concern (the outbox row remains pending).
    fn on_rename(&self, event: &RenameEvent);
}
static OBSERVERS: RwLock<Vec<Arc<dyn RenameObserver>>> = RwLock::new(Vec::new());
/// Registers a process-wide observer; returns false when the bounded list is full.
pub fn register_observer(observer: Arc<dyn RenameObserver>) -> bool {
    match OBSERVERS.write() {
        Ok(mut list) if list.len() < MAX_OBSERVERS => {
            list.push(observer);
            true
        }
        _ => false,
    }
}
pub(crate) fn notify(event: &RenameEvent) {
    if let Ok(list) = OBSERVERS.read() {
        for observer in list.iter() {
            observer.on_rename(event);
        }
    }
}
pub(crate) const COLUMNS: &str = "id,kind,media_type,series_id,movie_id,file_id,command_id,old_path,new_path,sidecar_count,occurred_at,dispatched_at";
fn from_row(r: &libsql::Row) -> Option<RenameEvent> {
    let owner = match r.get::<String>(2).ok()?.as_str() {
        "tv" => RenameOwner::Tv(r.get(3).ok()?),
        "movies" => RenameOwner::Movies(r.get(4).ok()?),
        _ => return None,
    };
    Some(RenameEvent {
        id: r.get(0).ok()?,
        event_type: "rename",
        kind: match r.get::<String>(1).ok()?.as_str() {
            "file" => RenameEventKind::File,
            "folder" => RenameEventKind::Folder,
            _ => return None,
        },
        owner,
        file_id: r.get(5).ok()?,
        command_id: r.get(6).ok()?,
        old_path: r.get(7).ok()?,
        new_path: r.get(8).ok()?,
        sidecar_count: r.get(9).ok()?,
        occurred_at: r.get(10).ok()?,
        dispatched: r.get::<Option<String>>(11).ok()?.is_some(),
    })
}
/// Oldest undelivered events first, bounded.
pub async fn pending(c: &Connection, limit: u16) -> Result<Vec<RenameEvent>, libsql::Error> {
    let mut rows = c
        .query(
            &format!("SELECT {COLUMNS} FROM rename_events WHERE dispatched_at IS NULL ORDER BY occurred_at,id LIMIT ?"),
            [i64::from(limit.min(500))],
        )
        .await?;
    let mut out = vec![];
    while let Some(r) = rows.next().await? {
        if let Some(e) = from_row(&r) {
            out.push(e);
        }
    }
    Ok(out)
}
/// Marks events delivered; idempotent.
pub async fn mark_dispatched(c: &Connection, ids: &[String]) -> Result<u64, libsql::Error> {
    let mut changed = 0;
    for id in ids.iter().take(500) {
        changed += c
            .execute(
                "UPDATE rename_events SET dispatched_at=CURRENT_TIMESTAMP WHERE id=? AND dispatched_at IS NULL",
                [id.as_str()],
            )
            .await?;
    }
    Ok(changed)
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RenameEventQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[serde(default = "page_size")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
fn page_size() -> u16 {
    50
}
fn fail(status: StatusCode, code: &'static str) -> axum::response::Response {
    (status, Json(ApiErrorEnvelope::new(code, code))).into_response()
}
async fn list(
    State(db): State<Arc<crate::db::Database>>,
    q: Result<Query<RenameEventQuery>, axum::extract::rejection::QueryRejection>,
) -> axum::response::Response {
    let Ok(Query(q)) = q else {
        return fail(StatusCode::BAD_REQUEST, "invalid_rename_event_query");
    };
    if !(1..=100).contains(&q.limit) || q.offset > 10_000 {
        return fail(StatusCode::BAD_REQUEST, "invalid_rename_event_query");
    }
    let media = q
        .media_type
        .map(|m| if m == MediaDomain::Tv { "tv" } else { "movies" });
    let run = async {
        let c = db.connect().await?;
        let total = c
            .query(
                "SELECT count(*) FROM rename_events WHERE ? IS NULL OR media_type=?",
                params![media, media],
            )
            .await?
            .next()
            .await?
            .map_or(Ok(0), |r| r.get::<i64>(0))?;
        let mut rows = c
            .query(
                &format!("SELECT {COLUMNS} FROM rename_events WHERE ? IS NULL OR media_type=? ORDER BY occurred_at DESC,id DESC LIMIT ? OFFSET ?"),
                params![media, media, i64::from(q.limit), i64::from(q.offset)],
            )
            .await?;
        let mut items = vec![];
        while let Some(r) = rows.next().await? {
            if let Some(e) = from_row(&r) {
                items.push(e);
            }
        }
        Ok::<_, libsql::Error>(ApiPage {
            items,
            total,
            limit: q.limit,
            offset: q.offset,
        })
    };
    match run.await {
        Ok(page) => Json(page).into_response(),
        Err(_) => fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rename_event_storage_error",
        ),
    }
}
pub fn router(db: Arc<crate::db::Database>) -> Router {
    Router::new()
        .route("/api/v1/rename/events", get(list))
        .with_state(db)
}
