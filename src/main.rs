use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use hrrdarr::{db::Database, snapshots};
use std::{env, net::SocketAddr, sync::Arc};
#[cfg(test)]
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db: Arc<Database>,
    provider_key: Option<Arc<hrrdarr::providers::CredentialKey>>,
}

use hrrdarr::api::{LegacyError, SnapshotOptions};

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
    let provider_key = match env::var("HRRDARR_PROVIDER_KEY") {
        Ok(value) => Some(Arc::new(hrrdarr::providers::CredentialKey::from_hex(
            &value,
        )?)),
        Err(env::VarError::NotPresent) => None,
        Err(_) => return Err("Invalid provider key environment value".into()),
    };
    let state = Arc::new(AppState { db, provider_key });
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
        .merge(hrrdarr::import::router(state.db.clone()))
        .merge(hrrdarr::providers::router(
            state.db.clone(),
            state.provider_key.clone(),
        ))
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
            Json(LegacyError {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let state = Arc::new(AppState {
            db: Arc::new(db),
            provider_key: None,
        });
        // Retain malformed snapshot rejection independently of the replaced import503 check.
        assert_eq!(
            migrate(
                State(state.clone()),
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
    async fn production_router_merges_import_routes() {
        // Execution is now covered through real scratch-media HTTP tests in import_api.rs,
        // replacing the former assertion that every execute request returns 503.
        let directory = std::env::temp_dir().join(format!("hrrdarr-routes-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let state = Arc::new(AppState {
            db: Arc::new(Database::open_local(directory.join("db")).await.unwrap()),
            provider_key: None,
        });
        let _routes = router(state.clone());
        drop(_routes);
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
