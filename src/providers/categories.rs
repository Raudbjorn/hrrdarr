//! Caps-only category discovery for configuring an indexer; no provider or status writes.
use super::*;
use std::time::Duration;

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum IndexerImplementation {
    Torznab,
    Newznab,
}
impl IndexerImplementation {
    fn name(self) -> &'static str {
        match self {
            Self::Torznab => "torznab",
            Self::Newznab => "newznab",
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct IndexerConnection {
    pub implementation: IndexerImplementation,
    pub endpoint: String,
    #[serde(default, deserialize_with = "change")]
    #[ts(as="Option<Credentials>",optional=nullable)]
    pub credentials: Change<Credentials>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CategoryDiscoveryInput {
    pub media_type: MediaDomain,
    #[serde(deserialize_with = "required_nullable")]
    pub source: Option<draft::ProviderDraftSource>,
    pub connection: IndexerConnection,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CategoryOrigin {
    Advertised,
    StandardFallback,
}
#[derive(Serialize, ts_rs::TS)]
pub struct CategoryDiscoveryError {
    pub code: String,
    pub message: String,
    pub retry_after_seconds: Option<u32>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct CategoryDiscoveryResult {
    pub source: Option<draft::ProviderDraftSource>,
    pub media_type: MediaDomain,
    pub origin: CategoryOrigin,
    pub options: Vec<indexer::CategoryOption>,
    pub discovery_error: Option<CategoryDiscoveryError>,
}
fn discovery_error(error: Error) -> CategoryDiscoveryError {
    match error {
        Error::Plain(_, code, message) => CategoryDiscoveryError {
            code: code.into(),
            message: message.into(),
            retry_after_seconds: None,
        },
        Error::RateLimited(seconds) => CategoryDiscoveryError {
            code: "rate_limited".into(),
            message: "Provider is rate limited".into(),
            retry_after_seconds: Some(seconds),
        },
    }
}
pub(super) async fn discover(
    State(context): State<Context>,
    query: std::result::Result<Query<draft::DraftQuery>, QueryRejection>,
    input: std::result::Result<Json<CategoryDiscoveryInput>, JsonRejection>,
) -> Result<Json<CategoryDiscoveryResult>> {
    query.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    tokio::time::timeout(Duration::from_secs(35), execute(context, input))
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}
async fn prepare(context: &Context, input: &mut CategoryDiscoveryInput) -> Result<draft::Prepared> {
    let connection = &mut input.connection;
    if connection.endpoint.len() > 2048
        || matches!(&connection.credentials,Change::Value(value) if !value.valid(connection.implementation.name()))
    {
        return Err(bad());
    }
    if connection.endpoint.trim().is_empty() {
        // Standard options before configuration require no destination or credential decryption.
        let source = draft::load_source(context, input.source, connection.implementation.name())
            .await?
            .map(|(provider, _)| provider);
        return Ok(draft::Prepared {
            source,
            credentials: None,
        });
    }
    validate_endpoint(&connection.endpoint)?;
    draft::resolve(
        context,
        input.source,
        connection.implementation.name(),
        &connection.endpoint,
        &mut connection.credentials,
    )
    .await
}
async fn execute(
    context: Context,
    mut input: CategoryDiscoveryInput,
) -> Result<Json<CategoryDiscoveryResult>> {
    let _permit = draft::DRAFT_GATE
        .try_acquire()
        .map_err(|_| http_error(http::HttpError::Busy).0)?;
    let prepared = tokio::time::timeout(draft::PREFLIGHT_TIMEOUT, prepare(&context, &mut input))
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)??;
    if input.connection.endpoint.trim().is_empty() {
        if let Some(provider) = &prepared.source {
            current_revision(&context.db, provider).await?;
        }
        return bounded(CategoryDiscoveryResult {
            source: input.source,
            media_type: input.media_type,
            origin: CategoryOrigin::StandardFallback,
            options: indexer::standard_categories(input.media_type),
            discovery_error: Some(CategoryDiscoveryError {
                code: "not_configured".into(),
                message: "No indexer endpoint is configured".into(),
                retry_after_seconds: None,
            }),
        });
    }
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    let operation = transport
        .operation(
            prepared
                .source
                .as_ref()
                .map_or_else(Uuid::new_v4, |provider| provider.id),
        )
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result = indexer::discover_categories(
            &operation,
            &input.connection.endpoint,
            &IndexerAccess::from_credentials(&prepared.credentials),
            input.media_type,
        )
        .await;
        if let Some(provider) = &prepared.source {
            current_revision(&context.db, provider).await?;
        }
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        let (origin, options, error) = match result {
            Ok(options) => (CategoryOrigin::Advertised, options, None),
            Err(
                error @ (indexer::IndexerError::InvalidRequest
                | indexer::IndexerError::Transport(
                    http::HttpError::Busy | http::HttpError::InvalidRequest,
                )),
            ) => return Err(indexer_error(error, &operation).0),
            Err(error) => (
                CategoryOrigin::StandardFallback,
                indexer::standard_categories(input.media_type),
                Some(discovery_error(indexer_error(error, &operation).0)),
            ),
        };
        bounded(CategoryDiscoveryResult {
            source: input.source,
            media_type: input.media_type,
            origin,
            options,
            discovery_error: error,
        })
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Semaphore;
    #[tokio::test]
    async fn cancelled_category_handlers_release_the_shared_draft_admission() {
        let _isolation = draft::DRAFT_TEST_LOCK.lock().await;
        let path = std::env::temp_dir().join(format!("hrrdarr-category-cancel-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let context = Context {
            db: db.clone(),
            key: None,
            transport: Arc::new(http::HttpClient::new()),
        };
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.add_permits(1);
                    let permit = release.acquire().await.unwrap();
                    permit.forget();
                    "<caps/>"
                }
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let input = || {
            serde_json::from_value::<CategoryDiscoveryInput>(serde_json::json!({"source":null,"media_type":"tv","connection":{"implementation":"torznab","endpoint":endpoint}})).unwrap()
        };
        let mut tasks = vec![];
        for _ in 0..4 {
            tasks.push(tokio::spawn(discover(
                State(context.clone()),
                Ok(Query(draft::DraftQuery {})),
                Ok(Json(input())),
            )));
        }
        let permits = tokio::time::timeout(Duration::from_secs(3), started.acquire_many(4))
            .await
            .unwrap()
            .unwrap();
        permits.forget();
        assert!(matches!(
            discover(
                State(context.clone()),
                Ok(Query(draft::DraftQuery {})),
                Ok(Json(input()))
            )
            .await,
            Err(Error::Plain(StatusCode::TOO_MANY_REQUESTS, _, _))
        ));
        let input=serde_json::from_value::<draft::ProviderDraftInput>(serde_json::json!({"source":null,"config":{"name":"draft","enabled":true,"priority":1,"settings":{"implementation":"torznab","endpoint":endpoint,"tv":{"categories":[5000],"anime_categories":[]},"movies":null}}})).unwrap();
        assert!(matches!(
            draft::test(
                State(context.clone()),
                Ok(Query(draft::DraftQuery {})),
                Ok(Json(input))
            )
            .await,
            Err(Error::Plain(StatusCode::TOO_MANY_REQUESTS, _, _))
        ));
        for task in tasks {
            task.abort();
            assert!(matches!(task.await,Err(error) if error.is_cancelled()));
        }
        assert_eq!(draft::DRAFT_GATE.available_permits(), 4);
        let conn = db.connect().await.unwrap();
        for table in ["providers", "provider_tests"] {
            assert_eq!(
                conn.query(&format!("SELECT count(*) FROM {table}"), ())
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
        }
        release.add_permits(16);
        server.abort();
        let _ = server.await;
        drop(conn);
        drop(context);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
}
