//! HTTP surface: `/api/v1/notifications` (the Sonarr "Connections" resource, domain-qualified).
use super::{
    dispatcher::{self, Dispatcher},
    error::{ApiError, Result},
    model::*,
    store::{self, Runtime},
};
use crate::api::MediaDomain;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_DELIVERIES: i64 = 200;

fn body<T>(v: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    v.map(|j| j.0)
        .map_err(|_| ApiError::invalid("Expected a valid notification JSON object"))
}

pub fn router(dispatcher: Arc<Dispatcher>) -> Router {
    Router::new()
        .route("/api/v1/notifications", get(list).post(create))
        .route("/api/v1/notifications/schema", get(schema))
        .route("/api/v1/notifications/test", post(test_draft))
        .route("/api/v1/notifications/testall", post(test_all))
        .route("/api/v1/notifications/deliveries", get(deliveries))
        .route(
            "/api/v1/notifications/{id}",
            get(detail).put(update).delete(remove),
        )
        .route("/api/v1/notifications/{id}/test", post(test_saved))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(dispatcher)
}

type State_ = State<Arc<Dispatcher>>;

async fn list(State(d): State_) -> Result<Json<NotificationList>> {
    Ok(Json(NotificationList {
        items: d.store.list().await?,
    }))
}
async fn schema() -> Json<ImplementationList> {
    Json(ImplementationList {
        items: super::catalog::schemas(),
    })
}
async fn detail(State(d): State_, Path(id): Path<String>) -> Result<Json<Notification>> {
    Ok(Json(d.store.get(&id).await?))
}
async fn create(
    State(d): State_,
    input: std::result::Result<Json<NotificationInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Notification>)> {
    Ok((
        StatusCode::CREATED,
        Json(d.store.create(&body(input)?).await?),
    ))
}
async fn update(
    State(d): State_,
    Path(id): Path<String>,
    input: std::result::Result<Json<NotificationInput>, JsonRejection>,
) -> Result<Json<Notification>> {
    Ok(Json(d.store.update(&id, &body(input)?).await?))
}
async fn remove(State(d): State_, Path(id): Path<String>) -> Result<StatusCode> {
    d.store.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_saved(State(d): State_, Path(id): Path<String>) -> Result<Json<TestResult>> {
    let id =
        Uuid::parse_str(&id).map_err(|_| ApiError::invalid("Notification ID must be a UUID"))?;
    let c = d.store.db.connect().await?;
    let rt = d
        .store
        .runtime(&c, &id.to_string())
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(run_test(&d, &rt).await))
}

async fn run_test(d: &Dispatcher, rt: &Runtime) -> TestResult {
    TestResult {
        id: Some(rt.notification.id).filter(|i| !i.is_nil()),
        name: rt.notification.name.clone(),
        results: d.test(rt).await,
    }
}

async fn test_draft(
    State(d): State_,
    input: std::result::Result<Json<TestInput>, JsonRejection>,
) -> Result<Json<TestResult>> {
    let input = body(input)?;
    let c = d.store.db.connect().await?;
    let (mut secrets, stored, id) = match input.existing_id {
        Some(id) => {
            let rt = d
                .store
                .runtime(&c, &id.to_string())
                .await?
                .ok_or_else(ApiError::not_found)?;
            if rt.notification.implementation != input.draft.implementation {
                return Err(ApiError::invalid(
                    "The implementation of a notification cannot change",
                ));
            }
            let names: BTreeSet<String> = rt.secrets.keys().cloned().collect();
            (rt.secrets, names, id)
        }
        None => (Default::default(), BTreeSet::new(), Uuid::nil()),
    };
    let v = store::validate(&input.draft, &stored)?;
    for (name, change) in &v.secret_changes {
        match change {
            Some(value) => {
                secrets.insert(name.clone(), value.clone());
            }
            None => {
                secrets.remove(name);
            }
        }
    }
    let rt = Runtime {
        notification: Notification {
            id,
            name: v.name.clone(),
            implementation: v.implementation.to_owned(),
            enabled: v.enabled,
            revision: 0,
            settings: v.settings.clone(),
            secrets_configured: secrets.keys().cloned().collect(),
            tv: v.tv.clone(),
            movies: v.movies.clone(),
            created_at: 0,
            updated_at: 0,
        },
        secrets,
    };
    Ok(Json(run_test(&d, &rt).await))
}

/// Tests every enabled notification concurrently (bounded by the dispatcher's test slots) within
/// one overall deadline, so the request ends before the client gives up. A notification that has
/// not finished by then reports `timeout`; its send may still have been delivered.
async fn test_all(State(d): State_) -> Result<Json<TestAllResult>> {
    let deadline = tokio::time::Instant::now() + dispatcher::TEST_ALL_DEADLINE;
    let c = d.store.db.connect().await?;
    let enabled: Vec<Notification> = d
        .store
        .list()
        .await?
        .into_iter()
        .filter(|n| n.enabled)
        .collect();
    let mut items: Vec<TestResult> = enabled
        .iter()
        .map(|n| TestResult {
            id: Some(n.id),
            name: n.name.clone(),
            results: dispatcher::failed_notification_tests(n, "timeout"),
        })
        .collect();
    let mut tasks = tokio::task::JoinSet::new();
    for (index, n) in enabled.iter().enumerate() {
        match d.store.runtime(&c, &n.id.to_string()).await {
            Ok(Some(rt)) => {
                let d = d.clone();
                tasks.spawn(async move { (index, d.test_until(&rt, deadline).await) });
            }
            _ => {
                items[index].results =
                    dispatcher::failed_notification_tests(n, "credentials_unavailable")
            }
        }
    }
    // Each task stops itself at the deadline; a task that panicked keeps its timeout entry.
    while let Some(joined) = tasks.join_next().await {
        if let Ok((index, results)) = joined {
            items[index].results = results;
        }
    }
    Ok(Json(TestAllResult { items }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryQuery {
    limit: Option<i64>,
}
async fn deliveries(
    State(d): State_,
    Query(q): Query<DeliveryQuery>,
) -> Result<Json<DeliveryList>> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=MAX_DELIVERIES).contains(&limit) {
        return Err(ApiError::invalid("limit must be between 1 and 200"));
    }
    let c = d.store.db.connect().await?;
    let mut rows = c
        .query(
            "SELECT d.id,d.event_sequence,d.notification_id,e.kind,d.media_type,d.state,d.attempts,d.next_attempt_at,d.last_error,d.created_at,d.completed_at FROM notification_deliveries d JOIN notification_events e ON e.sequence=d.event_sequence ORDER BY d.id DESC LIMIT ?",
            [limit],
        )
        .await?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().await? {
        items.push(Delivery {
            id: r.get(0)?,
            event_sequence: r.get(1)?,
            notification_id: Uuid::parse_str(&r.get::<String>(2)?).unwrap_or_default(),
            event_kind: r.get(3)?,
            media_type: MediaDomain::parse(&r.get::<String>(4)?).ok(),
            state: r.get(5)?,
            attempts: r.get(6)?,
            next_attempt_at: r.get(7)?,
            last_error: r.get(8)?,
            created_at: r.get(9)?,
            completed_at: r.get(10)?,
        });
    }
    Ok(Json(DeliveryList { items }))
}
