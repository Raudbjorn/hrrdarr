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
    metadata: Arc<hrrdarr::metadata::MetadataClient>,
    db: Arc<Database>,
    provider_key: Option<Arc<hrrdarr::providers::CredentialKey>>,
}

use hrrdarr::api::{LegacyError, SnapshotOptions};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","event":"process_failed",
                "phase":error.phase,"error_class":error.class,"storage":error.storage})
            );
            std::process::ExitCode::FAILURE
        }
    }
}

struct ProcessFailure {
    phase: &'static str,
    class: String,
    storage: Option<serde_json::Value>,
}
impl ProcessFailure {
    fn at(phase: &'static str, error: impl Into<hrrdarr::db::Error>) -> Self {
        let error = error.into();
        // Only type information crosses this boundary, never Display/Debug/source chains.
        let class = if let Some(error) = error.downcast_ref::<std::io::Error>() {
            format!("io_{:?}", error.kind())
        } else if let Some(error) = error.downcast_ref::<libsql::Error>() {
            match error {
                libsql::Error::ConnectionFailed(_) => "database_connection",
                libsql::Error::Hrana(_) => "remote_database_protocol",
                libsql::Error::SqliteFailure(_, _)
                | libsql::Error::RemoteSqliteFailure(_, _, _) => "database_sql",
                _ => "database_operation",
            }
            .into()
        } else if error.is::<hrrdarr::db::StorageCompatibilityError>() {
            "storage_compatibility".into()
        } else {
            "configuration_or_operation".into()
        };
        let storage = error
            .downcast_ref::<hrrdarr::db::StorageCompatibilityError>()
            .map(|e| {
                serde_json::json!({"capability":e.capability,
                "sqlite_version":e.engine.as_ref().map(|engine| &engine.sqlite_version),
                "sqlite_source_id":e.engine.as_ref().map(|engine| &engine.sqlite_source_id)})
            });
        Self {
            phase,
            class,
            storage,
        }
    }
}

async fn run() -> Result<(), ProcessFailure> {
    let db = if let (Ok(url), Ok(token)) =
        (env::var("TURSO_DATABASE_URL"), env::var("TURSO_AUTH_TOKEN"))
    {
        Database::open_remote(url, token)
            .await
            .map_err(|e| ProcessFailure::at("database_open", e))?
    } else {
        let path = env::var("HRRDARR_DATABASE_PATH").unwrap_or_else(|_| "hrrdarr.db".into());
        Database::open_local(path)
            .await
            .map_err(|e| ProcessFailure::at("database_open", e))?
    };
    hrrdarr::quality_profiles::initialize_defaults(&db)
        .await
        .map_err(|e| ProcessFailure::at("quality_profile_defaults", e))?;
    let db = Arc::new(db);
    if db.migration_backup().is_some() {
        println!("event=migration_backup_created");
    }
    let provider_key = match env::var("HRRDARR_PROVIDER_KEY") {
        Ok(value) => Some(Arc::new(
            hrrdarr::providers::CredentialKey::from_hex(&value)
                .map_err(|e| ProcessFailure::at("provider_key", e))?,
        )),
        Err(env::VarError::NotPresent) => None,
        Err(_) => {
            return Err(ProcessFailure::at(
                "provider_key",
                "invalid environment value",
            ));
        }
    };
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::new()
            .map_err(|e| ProcessFailure::at("metadata_client", e))?,
    );
    let state = Arc::new(AppState {
        db,
        provider_key,
        metadata,
    });
    let (app, refresh) = router_parts(state.clone());
    let addr: SocketAddr = env::var("HRRDARR_BIND")
        // 8787 collides with a live Readarr instance on hosts running the rest of the *arr
        // family alongside hrrdarr; 8760 avoids the whole 76xx-97xx range those apps use.
        .unwrap_or_else(|_| "127.0.0.1:8760".into())
        .parse()
        .map_err(|e| ProcessFailure::at("bind_address", e))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| ProcessFailure::at("listener_bind", e))?;
    let bound = listener
        .local_addr()
        .map_err(|e| ProcessFailure::at("listener_bind", e))?;
    println!("hrrdarr listening on http://{bound}");
    let runtime = if state.db.permits_local_imports() {
        Some(
            hrrdarr::commands::start_with_metadata(
                state.db.clone(),
                refresh,
                state.metadata.clone(),
            )
            .await
            .map_err(|e| ProcessFailure::at("command_worker", e))?,
        )
    } else {
        eprintln!("event=command_worker_disabled code=local_ownership_required");
        None
    };
    let result = axum::serve(listener, app).await;
    if let Some(runtime) = runtime {
        runtime.shutdown().await;
    }
    result.map_err(|e| ProcessFailure::at("http_server", e))?;
    Ok(())
}

#[cfg(test)]
fn router(state: Arc<AppState>) -> Router {
    router_parts(state).0
}

fn router_parts(state: Arc<AppState>) -> (Router, hrrdarr::providers::RefreshClient) {
    let (providers, refresh) =
        hrrdarr::providers::router_with_refresh(state.db.clone(), state.provider_key.clone());
    let app = Router::new()
        .route(
            "/api/v1/migrations",
            post(migrate).layer(DefaultBodyLimit::max(snapshots::MAX_SNAPSHOT_BYTES)),
        )
        .with_state(state.clone())
        .merge(hrrdarr::search::router(state.db.clone(), refresh.clone()))
        .merge(hrrdarr::qualities::router(state.db.clone()))
        .merge(hrrdarr::quality_profiles::router(state.db.clone()))
        .merge(hrrdarr::custom_formats::router(state.db.clone()))
        .merge(hrrdarr::tags::router(state.db.clone()))
        .merge(hrrdarr::episodes::router(state.db.clone()))
        .merge(hrrdarr::media_files::router(state.db.clone()))
        .merge(hrrdarr::library::router(state.db.clone()))
        .merge(hrrdarr::library::metadata_router(
            state.db.clone(),
            state.metadata.clone(),
        ))
        .merge(hrrdarr::root_folders::router(state.db.clone()))
        .merge(hrrdarr::filesystem::router())
        .merge(hrrdarr::remote_paths::router(state.db.clone()))
        .merge(hrrdarr::import::router(state.db.clone()))
        .merge(providers)
        .merge(hrrdarr::commands::router(state.db.clone()))
        .merge(hrrdarr::history::router(state.db.clone()))
        .merge(hrrdarr::blocklist::router(state.db.clone()))
        .merge(hrrdarr::naming::router(state.db.clone()))
        .merge(hrrdarr::parse::router(state.db.clone()));
    (app, refresh)
}

async fn migrate(
    State(state): State<Arc<AppState>>,
    Query(options): Query<SnapshotOptions>,
    bytes: axum::body::Bytes,
) -> Result<Json<snapshots::Report>, ApiError> {
    let result = if options.import_providers {
        snapshots::import_with_providers(
            &state.db,
            options.application,
            bytes.to_vec(),
            options.dry_run,
            state.provider_key.as_deref(),
        )
        .await
    } else {
        snapshots::import(
            &state.db,
            options.application,
            bytes.to_vec(),
            options.dry_run,
        )
        .await
    };
    result
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
            CREATE TABLE Indexers(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,EnableRss INTEGER);
            INSERT INTO Indexers VALUES(1,'Imported indexer','Torznab','TorznabSettings','{"baseUrl":"https://indexer.example","apiPath":"/api","apiKey":"SNAPSHOT_SECRET","categories":[5000],"animeCategories":[],"animeStandardFormatSearch":false}',1,1);
        "#).await.unwrap();
        drop(source);
        let bytes = std::fs::read(&source_path).unwrap();
        let db = Database::open_local(directory.join("destination.db"))
            .await
            .unwrap();
        let state = Arc::new(AppState {
            metadata: Arc::new(hrrdarr::metadata::MetadataClient::new().unwrap()),
            db: Arc::new(db),
            provider_key: None,
        });
        // Retain malformed snapshot rejection independently of the replaced import503 check.
        assert_eq!(
            migrate(
                State(state.clone()),
                Query(SnapshotOptions {
                    application: snapshots::Application::Sonarr,
                    dry_run: true,
                    import_providers: false,
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
                    import_providers: false,
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
        let missing_key = migrate(
            State(state.clone()),
            Query(SnapshotOptions {
                application: snapshots::Application::Sonarr,
                dry_run: false,
                import_providers: true,
            }),
            bytes.clone().into(),
        )
        .await
        .unwrap_err();
        assert_eq!(missing_key.status, StatusCode::BAD_REQUEST);
        assert!(!missing_key.message.contains("SNAPSHOT_SECRET"));
        let keyed_state = Arc::new(AppState {
            metadata: Arc::new(hrrdarr::metadata::MetadataClient::new().unwrap()),
            db: state.db.clone(),
            provider_key: Some(Arc::new(
                hrrdarr::providers::CredentialKey::from_hex(&"ab".repeat(32)).unwrap(),
            )),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(keyed_state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        for (query, expected_status, expected_providers) in [
            ("application=sonarr&dry_run=false", 200, 0),
            ("application=sonarr&import_providers=invalid", 400, 0),
            ("application=sonarr&import_providers=true", 200, 0),
            (
                "application=sonarr&import_providers=true&dry_run=false",
                200,
                1,
            ),
            (
                "application=sonarr&import_providers=true&dry_run=false",
                200,
                1,
            ),
        ] {
            let response = client
                .post(format!("http://{address}/api/v1/migrations?{query}"))
                .header("Content-Type", "application/octet-stream")
                .body(bytes.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected_status, "{query}");
            assert!(!response.text().await.unwrap().contains("SNAPSHOT_SECRET"));
            let conn = state.db.connect().await.unwrap();
            let row = conn
                .query(
                    "SELECT count(*), coalesce(sum(enabled),0) FROM providers",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.get::<i64>(0).unwrap(), expected_providers);
            assert_eq!(row.get::<i64>(1).unwrap(), 0);
        }
        server.abort();
        let _ = server.await;
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
            metadata: Arc::new(hrrdarr::metadata::MetadataClient::new().unwrap()),
            db: Arc::new(Database::open_local(directory.join("db")).await.unwrap()),
            provider_key: None,
        });
        let _routes = router(state.clone());
        drop(_routes);
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
