//! Test proposed settings without saving configuration or observations.
use super::*;
use std::{sync::LazyLock, time::Duration};
use tokio::sync::Semaphore;

// ponytail: Fixed admission bounds drafts until the command scheduler owns interactive test work.
static DRAFT_GATE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderDraftSource {
    pub id: Uuid,
    pub revision: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderDraftInput {
    #[serde(deserialize_with = "required_nullable")]
    pub source: Option<ProviderDraftSource>,
    pub config: ProviderInput,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderDraftResult {
    pub source: Option<ProviderDraftSource>,
    pub tested_at: i64,
    pub result: ProviderTestOutcome,
}
struct Prepared {
    source: Option<Provider>,
    credentials: Option<Credentials>,
}
async fn prepare(context: &Context, input: &mut ProviderDraftInput) -> Result<Prepared> {
    validate(&input.config)?;
    let source = if let Some(binding) = input.source {
        if !(1..=9007199254740991).contains(&binding.revision) {
            return Err(bad());
        }
        let conn = context.db.connect().await?;
        let tx = conn.transaction().await?;
        let result = read(&tx, &binding.id.to_string()).await;
        let (provider, bytes) = finish(tx, result).await?;
        if provider.revision != binding.revision
            || provider.settings.implementation() != input.config.settings.implementation()
        {
            return Err(conflict());
        }
        Some((provider, bytes))
    } else {
        None
    };
    let credentials = match std::mem::replace(&mut input.config.credentials, Change::Missing) {
        Change::Missing => match &source {
            Some((provider, bytes)) => {
                // Exact endpoint equality includes scheme, host, port and path. No inherited secret may change destinations implicitly.
                if bytes.is_some()
                    && provider.settings.endpoint() != input.config.settings.endpoint()
                {
                    return Err(Error::Plain(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "provider_credential_binding",
                        "Supply explicit draft credentials when changing the endpoint",
                    ));
                }
                unlock(context, provider, bytes.clone())?
            }
            None => None,
        },
        // These values stay in memory; unlike persisted edits, replacement/clear neither decrypt nor overwrite the saved bundle.
        Change::Value(credentials) => Some(credentials),
        Change::Null => None,
    };
    Ok(Prepared {
        source: source.map(|(provider, _)| provider),
        credentials,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftQuery {}
pub(super) async fn test(
    State(context): State<Context>,
    query: std::result::Result<Query<DraftQuery>, QueryRejection>,
    input: std::result::Result<Json<ProviderDraftInput>, JsonRejection>,
) -> Result<Json<ProviderDraftResult>> {
    query.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    execute(context, input).await
}
async fn execute(context: Context, input: ProviderDraftInput) -> Result<Json<ProviderDraftResult>> {
    // Includes preflight and the existing operation deadline; does not extend either nested bound.
    execute_for(context, input, Duration::from_secs(35)).await
}
async fn execute_for(
    context: Context,
    input: ProviderDraftInput,
    budget: Duration,
) -> Result<Json<ProviderDraftResult>> {
    tokio::time::timeout(budget, execute_inner(context, input))
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}
async fn execute_inner(
    context: Context,
    mut input: ProviderDraftInput,
) -> Result<Json<ProviderDraftResult>> {
    let _permit = DRAFT_GATE
        .try_acquire()
        .map_err(|_| http_error(http::HttpError::Busy).0)?;
    let prepared = tokio::time::timeout(PREFLIGHT_TIMEOUT, prepare(&context, &mut input))
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)??;
    let transport = context
        .transport
        .as_ref()
        .as_ref()
        .map_err(|e| http_error(*e).0)?;
    // The source lane retains saved-provider concurrency/cooldown. New drafts use internal IDs only and remain globally bounded.
    let operation = transport
        .operation(
            prepared
                .source
                .as_ref()
                .map_or_else(Uuid::new_v4, |provider| provider.id),
        )
        .map_err(|e| http_error(e).0)?;
    let work = async {
        let result = probe(&operation, &input.config.settings, &prepared.credentials).await;
        // Revision checks apply to completed protocol failures as well as successes; no observation is written.
        if let Some(provider) = &prepared.source {
            current_revision(&context.db, provider).await?;
        }
        operation.ensure_active().map_err(|e| http_error(e).0)?;
        let result = result.map_err(|(error, _)| error)?;
        let tested_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| corrupt())?
            .as_secs()
            .try_into()
            .map_err(|_| corrupt())?;
        bounded(ProviderDraftResult {
            source: input.source,
            tested_at,
            result,
        })
    };
    tokio::time::timeout_at(operation.deadline(), work)
        .await
        .map_err(|_| http_error(http::HttpError::Timeout).0)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_and_timed_out_drafts_release_admission_without_writes() {
        let path = std::env::temp_dir().join(format!("hrrdarr-draft-cancel-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
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
                    "2.8.3"
                }
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let context = Context {
            db: db.clone(),
            key: None,
            transport: Arc::new(http::HttpClient::new()),
        };
        let input = || {
            serde_json::from_value::<ProviderDraftInput>(serde_json::json!({"source":null,"config":{"name":"draft","enabled":false,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":null}}})).unwrap()
        };
        let mut tasks = Vec::new();
        for _ in 0..4 {
            tasks.push(tokio::spawn(execute(context.clone(), input())));
        }
        let permits = tokio::time::timeout(Duration::from_secs(3), started.acquire_many(4))
            .await
            .unwrap()
            .unwrap();
        permits.forget();
        assert!(matches!(
            execute(context.clone(), input()).await,
            Err(Error::Plain(StatusCode::TOO_MANY_REQUESTS, _, _))
        ));
        for task in tasks {
            task.abort();
            assert!(matches!(task.await,Err(error) if error.is_cancelled()));
        }
        assert_eq!(DRAFT_GATE.available_permits(), 4);
        let result = execute_for(context.clone(), input(), Duration::from_millis(200)).await;
        assert!(matches!(
            result,
            Err(Error::Plain(StatusCode::GATEWAY_TIMEOUT, _, _))
        ));
        assert_eq!(DRAFT_GATE.available_permits(), 4);
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
