//! HTTP surface: `/api/v1/importlist` (Sonarr "Import Lists" / Radarr "Lists", domain-qualified),
//! `/exclusions`, `/schema`, `/test`, `/sync` and per-list `/{id}/sync`, `/runs`, `/items`.
use super::{
    catalog,
    error::{ApiError, Result},
    exclusions,
    model::*,
    sync::{self, Engine},
};
use crate::api::MediaDomain;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{delete, get, post},
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_RUNS: i64 = 32;
const MAX_PAGE: u16 = 500;

fn body<T>(v: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    v.map(|j| j.0)
        .map_err(|_| ApiError::invalid("Expected a valid import list JSON object"))
}

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/api/v1/importlist", get(list).post(create))
        .route("/api/v1/importlist/schema", get(schema))
        .route("/api/v1/importlist/test", post(test_draft))
        .route("/api/v1/importlist/sync", post(sync_all))
        .route(
            "/api/v1/importlist/exclusions",
            get(list_exclusions).post(add_exclusion),
        )
        .route(
            "/api/v1/importlist/exclusions/bulk",
            delete(delete_exclusions),
        )
        .route(
            "/api/v1/importlist/exclusions/{id}",
            get(get_exclusion).delete(delete_exclusion),
        )
        .route(
            "/api/v1/importlist/{id}",
            get(detail).put(update).delete(remove),
        )
        .route("/api/v1/importlist/{id}/test", post(test_saved))
        .route("/api/v1/importlist/{id}/sync", post(sync_one))
        .route("/api/v1/importlist/{id}/runs", get(runs))
        .route("/api/v1/importlist/{id}/items", get(items))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(engine)
}

type S = State<Arc<Engine>>;

async fn list(State(e): S) -> Result<Json<ImportListCollection>> {
    Ok(Json(ImportListCollection {
        items: e.store.list().await?,
    }))
}
async fn schema() -> Json<ImplementationList> {
    Json(ImplementationList {
        items: catalog::schemas(),
    })
}
async fn detail(State(e): S, Path(id): Path<String>) -> Result<Json<ImportList>> {
    Ok(Json(e.store.get(&id).await?))
}
async fn create(
    State(e): S,
    input: std::result::Result<Json<ImportListInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ImportList>)> {
    Ok((
        StatusCode::CREATED,
        Json(e.store.create(&body(input)?).await?),
    ))
}
async fn update(
    State(e): S,
    Path(id): Path<String>,
    input: std::result::Result<Json<ImportListInput>, JsonRejection>,
) -> Result<Json<ImportList>> {
    Ok(Json(e.store.update(&id, &body(input)?).await?))
}
async fn remove(State(e): S, Path(id): Path<String>) -> Result<StatusCode> {
    e.store.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetches exactly as a sync does but never writes: the result is a count or a stable error class.
async fn run_test(e: &Engine, runtime: &super::providers::ListRuntime) -> TestResult {
    match catalog::fetch(&e.transport, runtime).await {
        Ok(items) => TestResult {
            ok: true,
            item_count: i64::try_from(items.len()).ok(),
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            item_count: None,
            error: Some(error.code().to_owned()),
        },
    }
}
async fn test_saved(State(e): S, Path(id): Path<String>) -> Result<Json<TestResult>> {
    let id =
        Uuid::parse_str(&id).map_err(|_| ApiError::invalid("Import list ID must be a UUID"))?;
    let c = e.store.db.connect().await?;
    let (_, runtime) = e
        .store
        .runtime(&c, &id.to_string())
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(run_test(&e, &runtime).await))
}
async fn test_draft(
    State(e): S,
    input: std::result::Result<Json<TestInput>, JsonRejection>,
) -> Result<Json<TestResult>> {
    let input = body(input)?;
    let runtime = e
        .store
        .draft_runtime(&input.draft, input.existing_id)
        .await?;
    Ok(Json(run_test(&e, &runtime).await))
}

async fn sync_one(State(e): S, Path(id): Path<String>) -> Result<(StatusCode, Json<Run>)> {
    let (run, _) = e.enqueue(&id, "manual").await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}
async fn sync_all(State(e): S) -> Result<(StatusCode, Json<SyncAll>)> {
    Ok((StatusCode::ACCEPTED, Json(e.enqueue_all("manual").await?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunQuery {
    limit: Option<i64>,
}
async fn runs(
    State(e): S,
    Path(id): Path<String>,
    Query(q): Query<RunQuery>,
) -> Result<Json<RunCollection>> {
    let id =
        Uuid::parse_str(&id).map_err(|_| ApiError::invalid("Import list ID must be a UUID"))?;
    let limit = q.limit.unwrap_or(10);
    if !(1..=MAX_RUNS).contains(&limit) {
        return Err(ApiError::invalid("limit must be between 1 and 32"));
    }
    let c = e.store.db.connect().await?;
    if super::store::read(&c, &id.to_string()).await?.is_none() {
        return Err(ApiError::not_found());
    }
    Ok(Json(RunCollection {
        items: sync::list_runs(&c, &id.to_string(), limit).await?,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemQuery {
    state: Option<String>,
    limit: Option<u16>,
    offset: Option<u32>,
}
async fn items(
    State(e): S,
    Path(id): Path<String>,
    Query(q): Query<ItemQuery>,
) -> Result<Json<ItemPage>> {
    let id =
        Uuid::parse_str(&id).map_err(|_| ApiError::invalid("Import list ID must be a UUID"))?;
    let (limit, offset) = (q.limit.unwrap_or(100), q.offset.unwrap_or(0));
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::invalid("limit must be between 1 and 500"));
    }
    if let Some(state) = &q.state {
        if ![
            "pending",
            "added",
            "existing",
            "excluded",
            "unresolved",
            "failed",
        ]
        .contains(&state.as_str())
        {
            return Err(ApiError::invalid("Unknown item state"));
        }
    }
    let c = e.store.db.connect().await?;
    let id = id.to_string();
    if super::store::read(&c, &id).await?.is_none() {
        return Err(ApiError::not_found());
    }
    let total = super::store::scalar(
        &c,
        "SELECT count(*) FROM import_list_items WHERE list_id=?1 AND (?2 IS NULL OR state=?2)",
        libsql::params![id.clone(), q.state.clone()],
    )
    .await?;
    let mut rows = c
        .query(
            "SELECT key,title,year,tvdb_id,tmdb_id,imdb_id,state,error_code,library_id FROM import_list_items WHERE list_id=?1 AND (?2 IS NULL OR state=?2) ORDER BY seq LIMIT ?3 OFFSET ?4",
            libsql::params![id, q.state.clone(), i64::from(limit), i64::from(offset)],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(Item {
            key: r.get(0)?,
            title: r.get(1)?,
            year: r.get(2)?,
            tvdb_id: r.get(3)?,
            tmdb_id: r.get(4)?,
            imdb_id: r.get(5)?,
            state: r.get(6)?,
            error_code: r.get(7)?,
            library_id: r.get(8)?,
        });
    }
    Ok(Json(ItemPage {
        items: out,
        total,
        limit,
        offset,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExclusionQuery {
    media_type: Option<MediaDomain>,
    limit: Option<u16>,
    offset: Option<u32>,
}
async fn list_exclusions(
    State(e): S,
    Query(q): Query<ExclusionQuery>,
) -> Result<Json<ExclusionPage>> {
    Ok(Json(
        exclusions::list(
            &e.store.db,
            q.media_type,
            q.limit.unwrap_or(100),
            q.offset.unwrap_or(0),
        )
        .await?,
    ))
}
async fn add_exclusion(
    State(e): S,
    input: std::result::Result<Json<ExclusionInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Exclusion>)> {
    let (row, created) = exclusions::add(&e.store.db, &body(input)?).await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(row),
    ))
}
async fn get_exclusion(State(e): S, Path(id): Path<i64>) -> Result<Json<Exclusion>> {
    Ok(Json(exclusions::get(&e.store.db, id).await?))
}
async fn delete_exclusion(State(e): S, Path(id): Path<i64>) -> Result<StatusCode> {
    exclusions::delete(&e.store.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn delete_exclusions(
    State(e): S,
    input: std::result::Result<Json<ExclusionBulkDelete>, JsonRejection>,
) -> Result<StatusCode> {
    exclusions::delete_many(&e.store.db, &body(input)?.ids).await?;
    Ok(StatusCode::NO_CONTENT)
}
