use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use hrrdarr::{
    db::{Database, MediaTarget},
    snapshots,
};
use libsql::params;
use serde::{Deserialize, Serialize};
use std::{env, net::SocketAddr, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db: Arc<Database>,
}

#[derive(Debug, Deserialize)]
struct ImportRequest {
    episode_id: i64,
    source: String,
    mode: String,
    destination: String,
}

#[derive(Debug, Serialize)]
struct Operation {
    id: Uuid,
    target: MediaTarget,
    status: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), hrrdarr::db::Error> {
    let db = if let (Ok(url), Ok(token)) =
        (env::var("TURSO_DATABASE_URL"), env::var("TURSO_AUTH_TOKEN"))
    {
        Database::open_remote(url, token).await?
    } else {
        let path = env::var("HRRDARR_DATABASE_PATH").unwrap_or_else(|_| "hrrdarr.db".into());
        Database::open_local(path).await?
    };
    let db = Arc::new(db);
    if let Some(path) = db.migration_backup() {
        println!("pre-migration recovery backup: {}", path.display());
    }
    let state = Arc::new(AppState { db });
    let app = router(state);
    let addr: SocketAddr = env::var("HRRDARR_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("hrrdarr listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/imports", post(import_preview))
        .route("/api/v1/imports/{id}/execute", post(import_execute))
        .route(
            "/api/v1/migrations",
            post(migrate).layer(DefaultBodyLimit::max(snapshots::MAX_SNAPSHOT_BYTES)),
        )
        .with_state(state.clone())
        .merge(hrrdarr::qualities::router(state.db.clone()))
        .merge(hrrdarr::quality_profiles::router(state.db.clone()))
        .merge(hrrdarr::episodes::router(state.db.clone()))
        .merge(hrrdarr::media_files::router(state.db.clone()))
        .merge(hrrdarr::library::router(state.db.clone()))
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
    let conn = state.db.connect().await?;
    if conn
        .query(
            "SELECT id FROM episodes WHERE id = ?1",
            params![req.episode_id],
        )
        .await?
        .next()
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("episode not found"));
    }
    conn.execute(
        "INSERT INTO operations (id, media_type, episode_id, source, mode, destination, status, message) VALUES (?1, 'episode', ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id.to_string(), req.episode_id, req.source, req.mode, req.destination, "preview", message.clone()],
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Operation {
            id,
            target: MediaTarget::Episode(req.episode_id),
            status: "preview",
            message,
        }),
    ))
}

// Keep execution unavailable until file and database recovery is implemented.
async fn import_execute() -> Result<Json<Operation>, ApiError> {
    Err(ApiError::unavailable(
        "import execution is unavailable until recoverable file/DB imports are implemented; previews are retained",
    ))
}

#[derive(Deserialize)]
struct SnapshotOptions {
    application: snapshots::Application,
    #[serde(default = "default_dry_run")]
    dry_run: bool,
}
fn default_dry_run() -> bool {
    true
}

async fn migrate(
    State(state): State<Arc<AppState>>,
    Query(options): Query<SnapshotOptions>,
    bytes: axum::body::Bytes,
) -> Result<Json<snapshots::Report>, ApiError> {
    snapshots::import(
        &state.db,
        options.application,
        bytes.to_vec(),
        options.dry_run,
    )
    .await
    .map(Json)
    .map_err(|error| ApiError::bad_request(error.to_string()))
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
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
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
    use super::*;

    #[test]
    fn rejects_unknown_transfer_mode() {
        assert!(!matches!("replace", "copy" | "move" | "hardlink"));
    }

    #[tokio::test]
    async fn snapshot_handler_previews_then_applies_uploaded_core_library() {
        let directory = std::env::temp_dir().join(format!("hrrdarr-upload-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let source_path = directory.join("backup.db");
        let source = libsql::Builder::new_local(&source_path)
            .build()
            .await
            .unwrap();
        source.connect().unwrap().execute_batch(r#"
            CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
            CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
            INSERT INTO Series VALUES(1,100,'Upload',2024,'/tv/Upload',1,'[]');
            CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
            CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
        "#).await.unwrap();
        drop(source);
        let bytes = std::fs::read(&source_path).unwrap();
        let db = Database::open_local(directory.join("destination.db"))
            .await
            .unwrap();
        let state = Arc::new(AppState { db: Arc::new(db) });
        for dry_run in [true, false] {
            let response = migrate(
                State(state.clone()),
                Query(SnapshotOptions {
                    application: snapshots::Application::Sonarr,
                    dry_run,
                }),
                bytes.clone().into(),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(response.applied, !dry_run);
            assert_eq!(response.mapped, 2); // The series and its optional settings sidecar map independently.
            // The legacy series route is exercised over HTTP in tests/library_api.rs.
            let count = state
                .db
                .connect()
                .await
                .unwrap()
                .query("SELECT count(*) FROM series", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap();
            assert_eq!(count, i64::from(!dry_run));
        }
        assert_eq!(bytes, std::fs::read(source_path).unwrap());
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn schema_handlers_preserve_paths_and_reject_unimplemented_writes() {
        let directory = std::env::temp_dir().join(format!("hrrdarr-handlers-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        {
            let db = Database::open_local(directory.join("library.db"))
                .await
                .unwrap();
            let conn = db.connect().await.unwrap();
            conn.execute_batch("INSERT INTO series (id,title,path) VALUES (1,'Series','/tv/Series');
                INSERT INTO seasons (series_id,number) VALUES (1,1);
                INSERT INTO episode_files (id,series_id,path) VALUES (1,1,'/tv/Series/episode.mkv');
                INSERT INTO episodes (id,series_id,season,number,title,episode_file_id) VALUES (1,1,1,1,'Episode',1);")
                .await.unwrap();
            let state = Arc::new(AppState { db: Arc::new(db) });
            // Construct the production merge to catch static/wildcard route conflicts.
            let _routes = router(state.clone());
            // Episode endpoint/path assertions now exercise the real router in tests/episode_api.rs.
            let request = |episode_id| {
                Json(ImportRequest {
                    episode_id,
                    source: "/download/file".into(),
                    mode: "copy".into(),
                    destination: "/tv/new.mkv".into(),
                })
            };
            let response = import_preview(State(state.clone()), request(999)).await;
            let error = match response {
                Err(error) => error,
                Ok(_) => panic!("unknown target accepted"),
            };
            assert_eq!(error.status, StatusCode::NOT_FOUND);
            assert_eq!(
                conn.query("SELECT count(*) FROM operations", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                0
            );
            let response = import_preview(State(state.clone()), request(1))
                .await
                .unwrap()
                .into_response();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let body = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                payload["target"],
                serde_json::json!({"media_type":"episode", "id":1})
            );
            assert_eq!(
                import_execute().await.unwrap_err().status,
                StatusCode::SERVICE_UNAVAILABLE
            );
            // Snapshot writes now validate uploads; malformed bytes return 400 rather than the former blanket 503.
            assert_eq!(
                migrate(
                    State(state),
                    Query(SnapshotOptions {
                        application: snapshots::Application::Sonarr,
                        dry_run: true
                    }),
                    axum::body::Bytes::from_static(b"not a database")
                )
                .await
                .unwrap_err()
                .status,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                conn.query("SELECT count(*) FROM operations WHERE status='preview'", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                1
            );
            assert_eq!(
                conn.query("SELECT count(*) FROM episodes", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                1
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
