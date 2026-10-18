//! Native administration for the three delivered providers. Media filters select whole providers.
use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::LazyLock,
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet, time::Instant};

const MAX_BULK: usize = 100;
const MAX_TESTS: usize = 32;
const TEST_CONCURRENCY: usize = 4;
const BATCH_SECONDS: u64 = 120;
const PREFLIGHT_SECONDS: u64 = 5;
// ponytail: One process-wide batch avoids queued fan-out; replace with the durable command scheduler when it owns provider testing.
static BATCH_GATE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Indexer,
    DownloadClient,
}
impl ProviderKind {
    fn matches(self, settings: &ProviderSettings) -> bool {
        matches!(
            (self, settings),
            (Self::DownloadClient, ProviderSettings::Qbittorrent { .. })
                | (
                    Self::Indexer,
                    ProviderSettings::Torznab { .. } | ProviderSettings::Newznab { .. }
                )
        )
    }
    fn database(self) -> &'static str {
        match self {
            Self::Indexer => "indexer",
            Self::DownloadClient => "download_client",
        }
    }
}
#[derive(Clone, Copy, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderFilter {
    pub media_type: MediaDomain,
    pub kind: ProviderKind,
}
impl ProviderFilter {
    fn matches(self, provider: &Provider) -> bool {
        let media = match (&provider.settings, self.media_type) {
            (
                ProviderSettings::Torznab { tv, .. } | ProviderSettings::Newznab { tv, .. },
                MediaDomain::Tv,
            ) => tv.is_some(),
            (
                ProviderSettings::Torznab { movies, .. } | ProviderSettings::Newznab { movies, .. },
                MediaDomain::Movies,
            ) => movies.is_some(),
            (ProviderSettings::Qbittorrent { tv, .. }, MediaDomain::Tv) => tv.is_some(),
            (ProviderSettings::Qbittorrent { movies, .. }, MediaDomain::Movies) => movies.is_some(),
        };
        media && self.kind.matches(&provider.settings)
    }
    fn media(self) -> &'static str {
        match self.media_type {
            MediaDomain::Tv => "tv",
            MediaDomain::Movies => "movies",
        }
    }
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderDefaults {
    Indexer {
        tv: TvIndexerScope,
        movies: MovieIndexerScope,
    },
    DownloadClient {
        imported_category: Option<String>,
        recent_priority: i8,
        older_priority: i8,
        initial_state: DownloadInitialState,
        content_layout: DownloadContentLayout,
        sequential_order: bool,
        first_last_first: bool,
        add_tags: bool,
    },
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum ProviderImplementation {
    Newznab,
    Qbittorrent,
    Torznab,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "media_type", rename_all = "snake_case")]
pub enum PresetScope {
    Tv { settings: TvIndexerScope },
    Movies { settings: MovieIndexerScope },
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderPreset {
    pub key: String,
    pub name: String,
    pub implementation: ProviderImplementation,
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub endpoint_hint: Option<String>,
    pub defaults: PresetScope,
}
fn preset_scope(media: MediaDomain, finder: bool) -> PresetScope {
    match media {
        MediaDomain::Tv => PresetScope::Tv {
            settings: TvIndexerScope {
                enable_rss: true,
                enable_automatic_search: true,
                enable_interactive_search: true,
                download_client_id: None,
                categories: if finder {
                    vec![5030, 5040, 5045]
                } else {
                    vec![5030, 5040]
                },
                anime_categories: vec![],
                anime_standard_format_search: false,
            },
        },
        MediaDomain::Movies => PresetScope::Movies {
            settings: MovieIndexerScope {
                enable_rss: true,
                enable_automatic_search: true,
                enable_interactive_search: true,
                download_client_id: None,
                categories: if finder {
                    vec![2030, 2040, 2045, 2050, 2060, 2070]
                } else {
                    vec![2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060]
                },
                remove_year: false,
            },
        },
    }
}
// Historical catalog metadata from both pinned references. No endpoint is contacted here.
fn presets(implementation: &ProviderImplementation, media: MediaDomain) -> Vec<ProviderPreset> {
    match implementation {
        ProviderImplementation::Qbittorrent => vec![],
        ProviderImplementation::Torznab => {
            if media == MediaDomain::Movies {
                vec![ProviderPreset {
            key: "torznab-jackett".into(), name: "Jackett".into(), implementation: ProviderImplementation::Torznab,
            enabled: false, endpoint: None,
            endpoint_hint: Some("Enter your Jackett host and indexer-specific Torznab endpoint; replace YOURINDEXER in /api/v2.0/indexers/YOURINDEXER/results/torznab/.".into()),
            defaults: preset_scope(media,false),
        }]
            } else {
                vec![]
            }
        }
        ProviderImplementation::Newznab => [
            ("dognzb", "DOGnzb", "https://api.dognzb.cr/api", None),
            (
                "drunkenslug",
                "DrunkenSlug",
                "https://drunkenslug.com/api",
                None,
            ),
            (
                "nzb-life",
                "Nzb.life",
                "https://api.nzb.life/api",
                Some(MediaDomain::Tv),
            ),
            (
                "nzb-su",
                "Nzb.su",
                "https://api.nzb.su/api",
                Some(MediaDomain::Movies),
            ),
            ("nzbcat", "NZBCat", "https://nzb.cat/api", None),
            (
                "nzbfinder",
                "NZBFinder.ws",
                "https://nzbfinder.ws/api",
                None,
            ),
            ("nzbgeek", "NZBgeek", "https://api.nzbgeek.info/api", None),
            (
                "nzbplanet",
                "nzbplanet.net",
                "https://api.nzbplanet.net/api",
                None,
            ),
            (
                "simplynzbs",
                "SimplyNZBs",
                "https://simplynzbs.com/api",
                None,
            ),
            (
                "tabula-rasa",
                "Tabula Rasa",
                "https://www.tabula-rasa.pw/api/v1/api",
                None,
            ),
            (
                "usenet-crawler",
                "Usenet Crawler",
                "https://www.usenet-crawler.com/api",
                Some(MediaDomain::Movies),
            ),
        ]
        .into_iter()
        .filter(|(_, _, _, listed)| listed.is_none_or(|domain| domain == media))
        .map(|(key, name, endpoint, _)| ProviderPreset {
            key: format!("newznab-{key}"),
            name: name.into(),
            implementation: ProviderImplementation::Newznab,
            enabled: false,
            endpoint: Some(endpoint.into()),
            endpoint_hint: None,
            defaults: preset_scope(media, key == "nzbfinder"),
        })
        .collect(),
    }
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderTemplate {
    pub implementation: ProviderImplementation,
    pub supported_media: Vec<MediaDomain>,
    pub enabled: bool,
    pub priority: u8,
    pub defaults: ProviderDefaults,
    pub presets: Vec<ProviderPreset>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderSchema {
    pub templates: Vec<ProviderTemplate>,
}
pub(super) async fn schema(
    query: std::result::Result<Query<ProviderFilter>, QueryRejection>,
) -> Result<Json<ProviderSchema>> {
    let filter = query.map_err(|_| bad())?.0;
    let templates = match filter.kind {
        ProviderKind::Indexer => [
            ProviderImplementation::Newznab,
            ProviderImplementation::Torznab,
        ]
        .into_iter()
        .map(|implementation| ProviderTemplate {
            presets: presets(&implementation, filter.media_type),
            implementation,
            supported_media: vec![MediaDomain::Tv, MediaDomain::Movies],
            enabled: false,
            priority: 1,
            defaults: ProviderDefaults::Indexer {
                tv: TvIndexerScope {
                    enable_rss: true,
                    enable_automatic_search: true,
                    enable_interactive_search: true,
                    download_client_id: None,
                    categories: vec![],
                    anime_categories: vec![],
                    anime_standard_format_search: false,
                },
                movies: MovieIndexerScope {
                    enable_rss: true,
                    enable_automatic_search: true,
                    enable_interactive_search: true,
                    download_client_id: None,
                    categories: vec![],
                    remove_year: false,
                },
            },
        })
        .collect(),
        ProviderKind::DownloadClient => vec![ProviderTemplate {
            implementation: ProviderImplementation::Qbittorrent,
            presets: vec![],
            supported_media: vec![MediaDomain::Tv, MediaDomain::Movies],
            enabled: false,
            priority: 1,
            defaults: ProviderDefaults::DownloadClient {
                imported_category: None,
                recent_priority: 0,
                older_priority: 0,
                initial_state: DownloadInitialState::default(),
                content_layout: DownloadContentLayout::default(),
                sequential_order: false,
                first_last_first: false,
                add_tags: false,
            },
        }],
    };
    bounded(ProviderSchema { templates })
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderSelectionItem {
    pub id: Uuid,
    pub revision: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderSelection {
    pub media_type: MediaDomain,
    pub kind: ProviderKind,
    pub items: Vec<ProviderSelectionItem>,
}
impl ProviderSelection {
    fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        if self.items.is_empty()
            || self.items.len() > MAX_BULK
            || self
                .items
                .iter()
                .any(|i| !(1..=9007199254740991).contains(&i.revision) || !ids.insert(i.id))
        {
            return Err(bad());
        }
        Ok(())
    }
    fn filter(&self) -> ProviderFilter {
        ProviderFilter {
            media_type: self.media_type,
            kind: self.kind,
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderChanges {
    #[ts(optional=nullable)]
    pub enabled: Option<bool>,
    #[ts(optional=nullable)]
    pub priority: Option<u8>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderBulkUpdate {
    #[serde(flatten)]
    pub selection: ProviderSelection,
    pub changes: ProviderChanges,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderBulkResult {
    pub items: Vec<Provider>,
}
async fn selected(
    context: &Context,
    tx: &Connection,
    selection: &ProviderSelection,
    delete: bool,
) -> Result<Vec<Provider>> {
    let mut providers = Vec::with_capacity(selection.items.len());
    for entry in &selection.items {
        let (provider, secret) = read(tx, &entry.id.to_string()).await?;
        if provider.revision != entry.revision {
            return Err(conflict());
        }
        if !selection.filter().matches(&provider) {
            return Err(bad());
        }
        credentials(
            context,
            &entry.id.to_string(),
            provider.settings.implementation(),
            secret,
            &if delete {
                Change::Null
            } else {
                Change::Missing
            },
        )?;
        providers.push(provider);
    }
    Ok(providers)
}
pub(super) async fn update(
    State(context): State<Context>,
    input: std::result::Result<Json<ProviderBulkUpdate>, JsonRejection>,
) -> Result<Json<ProviderBulkResult>> {
    let input = input.map_err(|_| bad())?.0;
    input.selection.validate()?;
    if input
        .selection
        .items
        .iter()
        .any(|item| item.revision == 9007199254740991)
    {
        return Err(bad());
    }
    if (input.changes.enabled.is_none() && input.changes.priority.is_none())
        || input
            .changes
            .priority
            .is_some_and(|p| !(1..=100).contains(&p))
    {
        return Err(bad());
    }
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let providers=selected(&context,&tx,&input.selection,false).await?;
        let mut changes = HealthConfigurationChanges::default();
        for provider in &providers { changes.capture(&tx, &provider.id.to_string(), Some(&provider.settings), None).await?; }
        let mut items=Vec::with_capacity(providers.len());
        for provider in providers {
            if tx.execute("UPDATE providers SET enabled=?,priority=?,revision=revision+1 WHERE id=? AND revision=?",params![i64::from(input.changes.enabled.unwrap_or(provider.enabled)),i64::from(input.changes.priority.unwrap_or(provider.priority)),provider.id.to_string(),provider.revision]).await?!=1 {return Err(conflict());}
            items.push(read(&tx,&provider.id.to_string()).await?.0);
        }
        changes.mark(&tx).await?;
        bounded(ProviderBulkResult{items})
    }.await;
    finish(tx, outcome).await
}
pub(super) async fn delete(
    State(context): State<Context>,
    input: std::result::Result<Json<ProviderSelection>, JsonRejection>,
) -> Result<StatusCode> {
    let input = input.map_err(|_| bad())?.0;
    input.validate()?;
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let providers = selected(&context, &tx, &input, true).await?;
        let mut changes = HealthConfigurationChanges::default();
        for provider in &providers {
            changes
                .capture(
                    &tx,
                    &provider.id.to_string(),
                    Some(&provider.settings),
                    None,
                )
                .await?;
        }
        for provider in providers {
            if tx
                .execute(
                    "DELETE FROM providers WHERE id=? AND revision=?",
                    params![provider.id.to_string(), provider.revision],
                )
                .await?
                != 1
            {
                return Err(conflict());
            }
        }
        changes.mark(&tx).await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}

#[derive(Serialize, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderBatchOutcome {
    Success {
        tested_at: i64,
    },
    Failure {
        code: String,
        message: String,
        retry_after_seconds: Option<u32>,
    },
    Changed,
    Timeout,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderBatchItem {
    pub provider_id: Uuid,
    pub revision: i64,
    pub outcome: ProviderBatchOutcome,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ProviderBatchResult {
    pub items: Vec<ProviderBatchItem>,
}
fn failure(error: Error) -> ProviderBatchOutcome {
    match error {
        Error::Plain(StatusCode::CONFLICT, _, _) => ProviderBatchOutcome::Changed,
        Error::Plain(_, code, message) => ProviderBatchOutcome::Failure {
            code: code.into(),
            message: message.into(),
            retry_after_seconds: None,
        },
        Error::RateLimited(seconds) => ProviderBatchOutcome::Failure {
            code: "rate_limited".into(),
            message: "Provider is rate limited".into(),
            retry_after_seconds: Some(seconds),
        },
    }
}
struct Candidate {
    id: Uuid,
    revision: i64,
    snapshot: Result<(Provider, Option<Credentials>)>,
}
async fn snapshots(context: &Context, filter: ProviderFilter) -> Result<Vec<Candidate>> {
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let mut rows=tx.query("SELECT p.id,p.revision FROM providers p WHERE p.enabled=1 AND ((?='download_client' AND p.implementation='qbittorrent') OR (?='indexer' AND p.implementation IN('torznab','newznab'))) AND EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=p.id AND s.media_type=?) ORDER BY p.priority,p.name,p.id LIMIT ?",params![filter.kind.database(),filter.kind.database(),filter.media(),(MAX_TESTS+1) as i64]).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push((row.get::<String>(0)?, row.get::<i64>(1)?));
    }
    if ids.len() > MAX_TESTS {
        tx.rollback().await?;
        return Err(Error::Plain(
            StatusCode::BAD_REQUEST,
            "provider_batch_too_large",
            "At most 32 enabled providers can be tested in one filtered batch",
        ));
    }
    let mut output = Vec::new();
    for (id, revision) in ids {
        let snapshot = match read(&tx, &id).await {
            Ok((provider, bytes)) => {
                unlock(context, &provider, bytes).map(|credentials| (provider, credentials))
            }
            Err(error) => Err(error),
        };
        output.push(Candidate {
            id: Uuid::parse_str(&id).map_err(|_| corrupt())?,
            revision,
            snapshot,
        });
    }
    tx.commit().await?;
    Ok(output)
}
pub(super) async fn test_all(
    State(context): State<Context>,
    input: std::result::Result<Json<ProviderFilter>, JsonRejection>,
) -> Result<Json<ProviderBatchResult>> {
    let filter = input.map_err(|_| bad())?.0;
    execute_batch(context, filter, Duration::from_secs(BATCH_SECONDS)).await
}
async fn execute_batch(
    context: Context,
    filter: ProviderFilter,
    budget: Duration,
) -> Result<Json<ProviderBatchResult>> {
    let permit = Arc::new(
        BATCH_GATE
            .clone()
            .try_acquire_owned()
            .map_err(|_| http_error(http::HttpError::Busy).0)?,
    );
    let deadline = Instant::now() + budget;
    let candidates = tokio::time::timeout(
        Duration::from_secs(PREFLIGHT_SECONDS),
        snapshots(&context, filter),
    )
    .await
    .map_err(|_| http_error(http::HttpError::Timeout).0)??;
    let mut items = Vec::new();
    let mut pending = VecDeque::new();
    for candidate in candidates {
        let index = items.len();
        let outcome = match candidate.snapshot {
            Ok(snapshot) => {
                pending.push_back((index, snapshot));
                ProviderBatchOutcome::Timeout
            }
            Err(error) => failure(error),
        };
        items.push(ProviderBatchItem {
            provider_id: candidate.id,
            revision: candidate.revision,
            outcome,
        });
    }
    let mut jobs = JoinSet::new();
    let mut indices = BTreeMap::new();
    loop {
        while jobs.len() < TEST_CONCURRENCY && Instant::now() < deadline {
            let Some((index, (provider, credentials))) = pending.pop_front() else {
                break;
            };
            let context = context.clone();
            let permit = permit.clone();
            let task = jobs.spawn(async move {
                let _permit = permit;
                current_revision(&context.db, &provider).await?;
                run_test(&context, provider, credentials).await
            });
            indices.insert(task.id(), index);
        }
        if jobs.is_empty() {
            break;
        }
        match tokio::time::timeout_at(deadline, jobs.join_next_with_id()).await {
            Ok(Some(Ok((id, result)))) => {
                let index = indices.remove(&id).ok_or_else(corrupt)?;
                items[index].outcome = match result {
                    Ok(result) => ProviderBatchOutcome::Success {
                        tested_at: result.0.tested_at,
                    },
                    Err(error) => failure(error),
                };
            }
            Ok(Some(Err(error))) => {
                if let Some(index) = indices.remove(&error.id()) {
                    items[index].outcome = failure(corrupt());
                }
            }
            Ok(None) => break,
            Err(_) => {
                jobs.abort_all();
                break;
            }
        }
    }
    // Dropping the set aborts all unfinished work. Each child retains the batch permit until it drops.
    bounded(ProviderBatchResult { items })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_revision_remains_deletable_and_requests_are_closed() {
        let selection:ProviderSelection=serde_json::from_value(serde_json::json!({"media_type":"tv","kind":"download_client","items":[{"id":Uuid::new_v4(),"revision":9007199254740991_i64}]})).unwrap();
        // Final safe revision cannot be incremented but must remain removable.
        selection.validate().unwrap();
        let malformed = serde_json::json!({"media_type":"tv","kind":"indexer","items":[],"changes":{"enabled":true},"unknown":true});
        assert!(serde_json::from_value::<ProviderBulkUpdate>(malformed).is_err());
    }
    #[tokio::test]
    async fn batch_deadline_and_cancellation_release_children_without_false_observations() {
        let path = std::env::temp_dir().join(format!("hrrdarr-admin-cancel-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        let started = Arc::new(tokio::sync::Notify::new());
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
                    started.notify_one();
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
        let config:ProviderInput=serde_json::from_value(serde_json::json!({"name":"deadline","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":endpoint,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":null}})).unwrap();
        let _ = create(State(context.clone()), Ok(Json(config)))
            .await
            .unwrap();
        let filter = ProviderFilter {
            media_type: MediaDomain::Tv,
            kind: ProviderKind::DownloadClient,
        };
        let batch = execute_batch(context.clone(), filter, Duration::from_millis(200))
            .await
            .unwrap();
        assert!(matches!(
            batch.0.items[0].outcome,
            ProviderBatchOutcome::Timeout
        ));
        async fn available() {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if BATCH_GATE.available_permits() == 1 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        available().await;
        // Drain the first request notification before proving a second, cancelled batch actually starts.
        let _ = tokio::time::timeout(Duration::from_millis(1), started.notified()).await;
        let task = tokio::spawn(execute_batch(
            context.clone(),
            filter,
            Duration::from_secs(60),
        ));
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        assert!(matches!(
            execute_batch(context.clone(), filter, Duration::from_secs(1)).await,
            Err(Error::Plain(StatusCode::TOO_MANY_REQUESTS, _, _))
        ));
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        available().await;
        let conn = db.connect().await.unwrap();
        assert_eq!(
            conn.query("SELECT count(*) FROM provider_tests", ())
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
        release.add_permits(8);
        server.abort();
        let _ = server.await;
        drop(conn);
        drop(context);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
}
