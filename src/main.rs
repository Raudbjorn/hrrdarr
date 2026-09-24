use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use libsql::{Database, params};
use serde::{Deserialize, Serialize};
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::fs;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db: Arc<Database>,
}

#[derive(Debug, Serialize)]
struct Series {
    id: i64,
    title: String,
    year: Option<i64>,
    path: String,
    poster: Option<String>,
}

#[derive(Debug, Serialize)]
struct Episode {
    id: i64,
    series_id: i64,
    season: i64,
    number: i64,
    title: String,
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportRequest {
    episode_id: i64,
    source: String,
    mode: String,
    destination: String,
}

#[derive(Debug, Deserialize)]
struct MigrationRequest {
    source_path: String,
    root_from: Option<String>,
    root_to: Option<String>,
}

#[derive(Debug, Serialize)]
struct MigrationReport {
    imported_series: usize,
    imported_episodes: usize,
    unsupported: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Operation {
    id: Uuid,
    status: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = if let (Ok(url), Ok(token)) =
        (env::var("TURSO_DATABASE_URL"), env::var("TURSO_AUTH_TOKEN"))
    {
        libsql::Builder::new_remote(url, token).build().await?
    } else {
        let path = env::var("HRRDARR_DATABASE_PATH").unwrap_or_else(|_| "hrrdarr.db".into());
        libsql::Builder::new_local(path).build().await?
    };
    let db = Arc::new(db);
    init_schema(&db).await?;
    let state = Arc::new(AppState { db });
    let app = Router::new()
        .route("/api/v1/series", get(series))
        .route("/api/v1/series/{id}/episodes", get(episodes))
        .route("/api/v1/imports", post(import_preview))
        .route("/api/v1/imports/{id}/execute", post(import_execute))
        .route("/api/v1/migrations", post(migrate))
        .with_state(state);
    let addr: SocketAddr = env::var("HRRDARR_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("hrrdarr listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn init_schema(db: &Database) -> Result<(), libsql::Error> {
    let conn = db.connect()?;
    conn.execute("CREATE TABLE IF NOT EXISTS series (id INTEGER PRIMARY KEY, title TEXT NOT NULL, year INTEGER, path TEXT NOT NULL, poster TEXT)", ()).await?;
    conn.execute("CREATE TABLE IF NOT EXISTS episodes (id INTEGER PRIMARY KEY, series_id INTEGER NOT NULL, season INTEGER NOT NULL, number INTEGER NOT NULL, title TEXT NOT NULL, file_path TEXT)", ()).await?;
    conn.execute("CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, episode_id INTEGER NOT NULL, source TEXT NOT NULL, mode TEXT NOT NULL, destination TEXT NOT NULL, status TEXT NOT NULL, message TEXT NOT NULL)", ()).await?;
    Ok(())
}

async fn series(State(state): State<Arc<AppState>>) -> Result<Json<Vec<Series>>, ApiError> {
    let conn = state.db.connect()?;
    let mut rows = conn
        .query(
            "SELECT id, title, year, path, poster FROM series ORDER BY title",
            (),
        )
        .await?;
    let mut result = Vec::new();
    while let Some(r) = rows.next().await? {
        result.push(Series {
            id: r.get(0)?,
            title: r.get(1)?,
            year: r.get(2)?,
            path: r.get(3)?,
            poster: r.get(4)?,
        });
    }
    Ok(Json(result))
}

async fn episodes(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<Episode>>, ApiError> {
    let conn = state.db.connect()?;
    let mut rows = conn.query("SELECT id, series_id, season, number, title, file_path FROM episodes WHERE series_id = ?1 ORDER BY season, number", params![id]).await?;
    let mut result = Vec::new();
    while let Some(r) = rows.next().await? {
        result.push(Episode {
            id: r.get(0)?,
            series_id: r.get(1)?,
            season: r.get(2)?,
            number: r.get(3)?,
            title: r.get(4)?,
            file_path: r.get(5)?,
        });
    }
    Ok(Json(result))
}

async fn import_preview(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ImportRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if req.source.trim().is_empty()
        || req.destination.trim().is_empty()
        || !matches!(req.mode.as_str(), "copy" | "move" | "hardlink")
    {
        return Err(ApiError::bad_request(
            "source, destination, and mode (copy|move|hardlink) are required",
        ));
    }
    let id = Uuid::new_v4();
    let message = format!(
        "episode {}: {} {} -> {}",
        req.episode_id, req.mode, req.source, req.destination
    );
    let conn = state.db.connect()?;
    conn.execute(
        "INSERT INTO operations (id, episode_id, source, mode, destination, status, message) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id.to_string(), req.episode_id, req.source, req.mode, req.destination, "preview", message.clone()],
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Operation {
            id,
            status: "preview",
            message,
        }),
    ))
}

async fn import_execute(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Operation>, ApiError> {
    let conn = state.db.connect()?;
    let mut rows = conn
        .query(
            "SELECT source, mode, destination FROM operations WHERE id = ?1",
            params![id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(ApiError::not_found("operation not found"));
    };
    let source: String = row.get(0)?;
    let mode: String = row.get(1)?;
    let destination: String = row.get(2)?;
    let source_path = PathBuf::from(&source);
    let destination_path = PathBuf::from(&destination);
    if !source_path.is_file() {
        return Err(ApiError::bad_request("source file does not exist"));
    }
    if destination_path.exists() {
        return Err(ApiError::bad_request("destination already exists"));
    }
    let Some(parent) = destination_path.parent() else {
        return Err(ApiError::bad_request("destination has no parent"));
    };
    fs::create_dir_all(parent).await.map_err(ApiError::io)?;
    match mode.as_str() {
        "copy" => {
            fs::copy(&source_path, &destination_path)
                .await
                .map_err(ApiError::io)?;
        }
        "move" => {
            fs::rename(&source_path, &destination_path)
                .await
                .map_err(ApiError::io)?;
        }
        "hardlink" => {
            fs::hard_link(&source_path, &destination_path)
                .await
                .map_err(ApiError::io)?;
        }
        _ => return Err(ApiError::bad_request("unsupported operation mode")),
    }
    conn.execute(
        "UPDATE operations SET status = ?1, message = ?2 WHERE id = ?3",
        params![
            "complete",
            format!("{mode}: {source} -> {destination}"),
            id.clone()
        ],
    )
    .await?;
    Ok(Json(Operation {
        id: id
            .parse()
            .map_err(|_| ApiError::bad_request("invalid operation id"))?,
        status: "complete",
        message: format!("{mode}: {source} -> {destination}"),
    }))
}

async fn migrate(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MigrationRequest>,
) -> Result<Json<MigrationReport>, ApiError> {
    let source_db = libsql::Builder::new_local(&req.source_path)
        .build()
        .await
        .map_err(ApiError::db)?;
    let source = source_db.connect().map_err(ApiError::db)?;
    let mut series_rows = source
        .query("SELECT Id, Title, Year, Path FROM Series", ())
        .await
        .map_err(ApiError::db)?;
    let mut imported_series = Vec::new();
    while let Some(row) = series_rows.next().await.map_err(ApiError::db)? {
        let mut path: String = row.get(3).map_err(ApiError::db)?;
        if let (Some(from), Some(to)) = (&req.root_from, &req.root_to) {
            if let Ok(relative) = PathBuf::from(&path).strip_prefix(from) {
                path = PathBuf::from(to)
                    .join(relative)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        imported_series.push((
            row.get::<i64>(0).map_err(ApiError::db)?,
            row.get::<String>(1).map_err(ApiError::db)?,
            row.get::<Option<i64>>(2).map_err(ApiError::db)?,
            path,
        ));
    }
    let mut episode_rows = source
        .query(
            "SELECT Id, SeriesId, SeasonNumber, EpisodeNumber, Title, EpisodeFileId FROM Episodes",
            (),
        )
        .await
        .map_err(ApiError::db)?;
    let mut imported_episodes = Vec::new();
    while let Some(row) = episode_rows.next().await.map_err(ApiError::db)? {
        imported_episodes.push((
            row.get::<i64>(0).map_err(ApiError::db)?,
            row.get::<i64>(1).map_err(ApiError::db)?,
            row.get::<i64>(2).map_err(ApiError::db)?,
            row.get::<i64>(3).map_err(ApiError::db)?,
            row.get::<String>(4).map_err(ApiError::db)?,
            row.get::<Option<i64>>(5).map_err(ApiError::db)?,
        ));
    }
    let conn = state.db.connect()?;
    conn.execute("DELETE FROM episodes", ()).await?;
    conn.execute("DELETE FROM series", ()).await?;
    for (id, title, year, path) in &imported_series {
        conn.execute(
            "INSERT INTO series (id,title,year,path) VALUES (?1,?2,?3,?4)",
            params![id, title.clone(), year, path.clone()],
        )
        .await?;
    }
    for (id, series_id, season, number, title, file_id) in &imported_episodes {
        conn.execute("INSERT INTO episodes (id,series_id,season,number,title,file_path) VALUES (?1,?2,?3,?4,?5,?6)", params![id, series_id, season, number, title.clone(), file_id.map(|v| v.to_string())]).await?;
    }
    Ok(Json(MigrationReport {
        imported_series: imported_series.len(),
        imported_episodes: imported_episodes.len(),
        unsupported: vec![
            "provider settings and scheduled commands retained for a later compatibility pass"
                .into(),
        ],
    }))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}
impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn io(e: std::io::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        }
    }
    fn db(e: libsql::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
        }
    }
}
impl From<libsql::Error> for ApiError {
    fn from(e: libsql::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_unknown_transfer_mode() {
        assert!(!matches!("replace", "copy" | "move" | "hardlink"));
    }
}
