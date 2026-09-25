//! Persisted first-pair configuration. Saving configuration performs no network requests.
mod credentials;
use crate::{
    api::{ApiErrorEnvelope, ApiPage, MediaDomain},
    db::Database,
    library::{Change, change},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
pub use credentials::{CredentialKey, Credentials};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.2))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "{}",
            serde_json::json!({"level":"ERROR","event":"provider_database_error","correlation_id":Uuid::new_v4(),"error_class":format!("{:?}",std::mem::discriminant(&error))})
        );
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider_database_error",
            "Provider operation failed; no partial configuration committed",
        )
    }
}
type Result<T> = std::result::Result<T, Error>;
fn bad() -> Error {
    Error(
        StatusCode::BAD_REQUEST,
        "invalid_provider_config",
        "Invalid provider configuration",
    )
}
fn conflict() -> Error {
    Error(
        StatusCode::CONFLICT,
        "provider_revision_conflict",
        "Provider revision or immutable implementation conflicts",
    )
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "provider_not_found",
        "Provider does not exist",
    )
}
fn locked() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "provider_credentials_locked",
        "Provider credentials cannot be unlocked; check the independently stored key",
    )
}
fn corrupt() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid_stored_provider",
        "Stored provider configuration is invalid",
    )
}

#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TvIndexerScope {
    pub categories: Vec<u32>,
    pub anime_categories: Vec<u32>,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MovieIndexerScope {
    pub categories: Vec<u32>,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct DownloadScope {
    pub category: String,
    #[serde(deserialize_with = "required_nullable")]
    pub imported_category: Option<String>,
    pub recent_priority: i8,
    pub older_priority: i8,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "implementation", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderSettings {
    Torznab {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TvIndexerScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<MovieIndexerScope>,
    },
    Newznab {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<TvIndexerScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<MovieIndexerScope>,
    },
    Qbittorrent {
        endpoint: String,
        #[serde(deserialize_with = "required_nullable")]
        tv: Option<DownloadScope>,
        #[serde(deserialize_with = "required_nullable")]
        movies: Option<DownloadScope>,
    },
}
impl ProviderSettings {
    fn implementation(&self) -> &'static str {
        match self {
            Self::Torznab { .. } => "torznab",
            Self::Newznab { .. } => "newznab",
            Self::Qbittorrent { .. } => "qbittorrent",
        }
    }
    fn endpoint(&self) -> &str {
        match self {
            Self::Torznab { endpoint, .. }
            | Self::Newznab { endpoint, .. }
            | Self::Qbittorrent { endpoint, .. } => endpoint,
        }
    }
    fn validate(&self) -> Result<()> {
        let endpoint = self.endpoint();
        let url = url::Url::parse(endpoint).map_err(|_| bad())?;
        if endpoint.trim() != endpoint
            || endpoint.contains('\\')
            || endpoint.len() > 2048
            || endpoint.chars().any(char::is_control)
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(bad());
        }
        let categories = |c: &[u32]| {
            c.len() <= 64
                && c.iter().all(|n| (1..=i32::MAX as u32).contains(n))
                && c.iter().collect::<BTreeSet<_>>().len() == c.len()
        };
        match self {
            Self::Torznab { tv, movies, .. } | Self::Newznab { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                if tv.as_ref().is_some_and(|s| {
                    !categories(&s.categories)
                        || !categories(&s.anime_categories)
                        || (s.categories.is_empty() && s.anime_categories.is_empty())
                }) || movies
                    .as_ref()
                    .is_some_and(|s| s.categories.is_empty() || !categories(&s.categories))
                {
                    return Err(bad());
                }
            }
            Self::Qbittorrent { tv, movies, .. } => {
                if tv.is_none() && movies.is_none() {
                    return Err(bad());
                }
                let category = |c: &str| {
                    !c.trim().is_empty()
                        && c.len() <= 64
                        && !c.chars().any(char::is_control)
                        && !c.contains('\\')
                        && !c.contains("//")
                        && !c.starts_with('/')
                        && !c.ends_with('/')
                };
                for s in tv.iter().chain(movies.iter()) {
                    if !category(&s.category)
                        || s.imported_category
                            .as_ref()
                            .is_some_and(|c| !category(c) || c == &s.category)
                        || !(0..=1).contains(&s.recent_priority)
                        || !(0..=1).contains(&s.older_priority)
                    {
                        return Err(bad());
                    }
                }
                if let (Some(tv), Some(movies)) = (tv, movies) {
                    let tvcats = std::iter::once(&tv.category).chain(tv.imported_category.iter());
                    let mcats: Vec<_> = std::iter::once(&movies.category)
                        .chain(movies.imported_category.iter())
                        .collect();
                    if tvcats.into_iter().any(|c| mcats.contains(&c)) {
                        return Err(bad());
                    }
                }
            }
        }
        Ok(())
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderInput {
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
    pub settings: ProviderSettings,
    #[serde(default, deserialize_with = "change")]
    #[ts(as="Option<Credentials>",optional=nullable)]
    pub credentials: Change<Credentials>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderUpdate {
    pub revision: i64,
    #[serde(flatten)]
    pub config: ProviderInput,
}
#[derive(Serialize, ts_rs::TS)]
pub struct Provider {
    pub id: Uuid,
    pub revision: i64,
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
    pub settings: ProviderSettings,
    pub has_credentials: bool,
    pub test_supported: bool,
    pub test_status: TestStatus,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    NeverTested,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderQuery {
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
}
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
fn default_limit() -> u16 {
    50
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderRevision {
    pub revision: i64,
}
#[derive(Clone)]
struct Context {
    db: Arc<Database>,
    key: Option<Arc<CredentialKey>>,
}
pub fn router(db: Arc<Database>, key: Option<Arc<CredentialKey>>) -> Router {
    Router::new()
        .route("/api/v1/providers", get(list).post(create))
        .route(
            "/api/v1/providers/{id}",
            get(detail).put(update).delete(delete),
        )
        .route("/api/v1/providers/{id}/test", post(test))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .with_state(Context { db, key })
}
fn id(value: String) -> Result<String> {
    Uuid::parse_str(&value)
        .map(|v| v.to_string())
        .map_err(|_| bad())
}
fn validate(input: &ProviderInput) -> Result<()> {
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
        || !(1..=100).contains(&input.priority)
    {
        return Err(bad());
    }
    input.settings.validate()?;
    if let Change::Value(c) = &input.credentials {
        if !c.valid(input.settings.implementation()) {
            return Err(bad());
        }
    }
    Ok(())
}
async fn read(conn: &Connection, id: &str) -> Result<(Provider, Option<Vec<u8>>)> {
    let row=conn.query("SELECT implementation,name,enabled,priority,revision,endpoint,credentials FROM providers WHERE id=?",[id]).await?.next().await?.ok_or_else(missing)?;
    let implementation: String = row.get(0)?;
    let endpoint: String = row.get(5)?;
    let mut scopes=conn.query("SELECT media_type,categories,anime_categories,category,imported_category,recent_priority,older_priority FROM provider_scopes WHERE provider_id=?",[id]).await?;
    let mut tv_index = None;
    let mut movie_index = None;
    let mut tv_client = None;
    let mut movie_client = None;
    while let Some(scope) = scopes.next().await? {
        let media: String = scope.get(0)?;
        if implementation == "qbittorrent" {
            let item = DownloadScope {
                category: scope.get(3)?,
                imported_category: scope.get(4)?,
                recent_priority: scope.get::<i64>(5)?.try_into().map_err(|_| corrupt())?,
                older_priority: scope.get::<i64>(6)?.try_into().map_err(|_| corrupt())?,
            };
            if media == "tv" {
                tv_client = Some(item)
            } else {
                movie_client = Some(item)
            }
        } else {
            let cats: Vec<u32> =
                serde_json::from_str(&scope.get::<String>(1)?).map_err(|_| corrupt())?;
            if media == "tv" {
                tv_index = Some(TvIndexerScope {
                    categories: cats,
                    anime_categories: serde_json::from_str(&scope.get::<String>(2)?)
                        .map_err(|_| corrupt())?,
                })
            } else {
                movie_index = Some(MovieIndexerScope { categories: cats })
            }
        }
    }
    let settings = match implementation.as_str() {
        "torznab" => ProviderSettings::Torznab {
            endpoint,
            tv: tv_index,
            movies: movie_index,
        },
        "newznab" => ProviderSettings::Newznab {
            endpoint,
            tv: tv_index,
            movies: movie_index,
        },
        "qbittorrent" => ProviderSettings::Qbittorrent {
            endpoint,
            tv: tv_client,
            movies: movie_client,
        },
        _ => return Err(corrupt()),
    };
    settings.validate().map_err(|_| corrupt())?;
    let credentials: Option<Vec<u8>> = row.get(6)?;
    Ok((
        Provider {
            id: Uuid::parse_str(id).map_err(|_| corrupt())?,
            revision: row.get(4)?,
            name: row.get(1)?,
            enabled: row.get::<i64>(2)? == 1,
            priority: row.get::<i64>(3)?.try_into().map_err(|_| corrupt())?,
            settings,
            has_credentials: credentials.is_some(),
            test_supported: false,
            test_status: TestStatus::NeverTested,
        },
        credentials,
    ))
}
async fn write_scopes(conn: &Connection, id: &str, settings: &ProviderSettings) -> Result<()> {
    let implementation = settings.implementation();
    conn.execute("DELETE FROM provider_scopes WHERE provider_id=?", [id])
        .await?;
    match settings {
        ProviderSettings::Torznab { tv, movies, .. }
        | ProviderSettings::Newznab { tv, movies, .. } => {
            for (domain, categories, anime) in tv
                .iter()
                .map(|s| ("tv", &s.categories, s.anime_categories.as_slice()))
                .chain(movies.iter().map(|s| ("movies", &s.categories, &[][..])))
            {
                let categories = serde_json::to_string(categories).map_err(|_| bad())?;
                let anime = serde_json::to_string(anime).map_err(|_| bad())?;
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories) VALUES(?,?,?,?,?)",params![id,implementation,domain,categories,anime]).await?;
            }
        }
        ProviderSettings::Qbittorrent { tv, movies, .. } => {
            for (domain, s) in tv
                .iter()
                .map(|s| ("tv", s))
                .chain(movies.iter().map(|s| ("movies", s)))
            {
                conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,imported_category,recent_priority,older_priority) VALUES(?,?,?,?,?,?,?)",params![id,implementation,domain,s.category.clone(),s.imported_category.clone(),i64::from(s.recent_priority),i64::from(s.older_priority)]).await?;
            }
        }
    }
    Ok(())
}
fn credentials(
    context: &Context,
    id: &str,
    implementation: &str,
    old: Option<Vec<u8>>,
    change: &Change<Credentials>,
) -> Result<Option<Vec<u8>>> {
    // Authenticate before *any* mutation of a credential-bearing row: a wrong key cannot erase it.
    if let Some(bytes) = &old {
        context
            .key
            .as_ref()
            .ok_or_else(locked)?
            .open(id, implementation, bytes)
            .map_err(|_| locked())?;
    }
    match change {
        Change::Missing => Ok(old),
        Change::Null => Ok(None),
        Change::Value(c) => context
            .key
            .as_ref()
            .ok_or_else(locked)?
            .seal(id, implementation, c)
            .map(Some)
            .map_err(|_| locked()),
    }
}
async fn create(
    State(context): State<Context>,
    input: std::result::Result<Json<ProviderInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Provider>)> {
    let input = input.map_err(|_| bad())?.0;
    validate(&input)?;
    let id = Uuid::new_v4().to_string();
    let secret = credentials(
        &context,
        &id,
        input.settings.implementation(),
        None,
        &input.credentials,
    )?;
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
    tx.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials) VALUES(?,?,?,?,?,1,1,?,?)",params![id.clone(),input.settings.implementation(),input.name,i64::from(input.enabled),i64::from(input.priority),input.settings.endpoint(),secret]).await?;
    write_scopes(&tx, &id, &input.settings).await?;
    Ok((StatusCode::CREATED, bounded(read(&tx, &id).await?.0)?))
    }.await;
    finish(tx, outcome).await
}
async fn update(
    State(context): State<Context>,
    Path(value): Path<String>,
    input: std::result::Result<Json<ProviderUpdate>, JsonRejection>,
) -> Result<Json<Provider>> {
    let id = id(value)?;
    let input = input.map_err(|_| bad())?.0;
    validate(&input.config)?;
    if !(1..9007199254740991).contains(&input.revision) {
        return Err(bad());
    }
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
    let (old, secret) = read(&tx, &id).await?;
    if old.revision != input.revision
        || old.settings.implementation() != input.config.settings.implementation()
    {
        return Err(conflict());
    }
    let secret = credentials(
        &context,
        &id,
        old.settings.implementation(),
        secret,
        &input.config.credentials,
    )?;
    let changed=tx.execute("UPDATE providers SET name=?,enabled=?,priority=?,revision=revision+1,endpoint=?,credentials=? WHERE id=? AND revision=?",params![input.config.name,i64::from(input.config.enabled),i64::from(input.config.priority),input.config.settings.endpoint(),secret,id.clone(),input.revision]).await?;
    if changed != 1 {
        return Err(conflict());
    }
    write_scopes(&tx, &id, &input.config.settings).await?;
    bounded(read(&tx, &id).await?.0)
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(context): State<Context>,
    Path(value): Path<String>,
) -> Result<Json<Provider>> {
    let id = id(value)?;
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let result = read(&tx, &id).await?.0;
    tx.commit().await?;
    bounded(result)
}
async fn list(
    State(context): State<Context>,
    query: std::result::Result<Query<ProviderQuery>, QueryRejection>,
) -> Result<Json<ApiPage<Provider>>> {
    let query = query.map_err(|_| bad())?.0;
    if !(1..=100).contains(&query.limit) {
        return Err(bad());
    }
    let domain = query.media_type.map(|d| match d {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    });
    let conn = context.db.connect().await?;
    let tx = conn.transaction().await?;
    let total=tx.query("SELECT count(*) FROM providers p WHERE ? IS NULL OR EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=p.id AND s.media_type=?)",params![domain,domain]).await?.next().await?.ok_or_else(corrupt)?.get(0)?;
    let mut rows=tx.query("SELECT p.id FROM providers p WHERE ? IS NULL OR EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=p.id AND s.media_type=?) ORDER BY p.priority,p.name,p.id LIMIT ? OFFSET ?",params![domain,domain,i64::from(query.limit),i64::from(query.offset)]).await?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().await? {
        items.push(read(&tx, &row.get::<String>(0)?).await?.0);
    }
    tx.commit().await?;
    bounded(ApiPage {
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    })
}
async fn delete(
    State(context): State<Context>,
    Path(value): Path<String>,
    query: std::result::Result<Query<ProviderRevision>, QueryRejection>,
) -> Result<StatusCode> {
    let id = id(value)?;
    let revision = query.map_err(|_| bad())?.0.revision;
    let conn = context.db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let (old, secret) = read(&tx, &id).await?;
        if old.revision != revision {
            return Err(conflict());
        }
        credentials(
            &context,
            &id,
            old.settings.implementation(),
            secret,
            &Change::Null,
        )?;
        if tx
            .execute(
                "DELETE FROM providers WHERE id=? AND revision=?",
                params![id, revision],
            )
            .await?
            != 1
        {
            return Err(conflict());
        }
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
async fn test(State(context): State<Context>, Path(value): Path<String>) -> Result<StatusCode> {
    let _ = detail(State(context), Path(value)).await?;
    Err(Error(
        StatusCode::NOT_IMPLEMENTED,
        "provider_test_not_implemented",
        "No network test is implemented for this provider yet",
    ))
}

// Bound serialization too, so future configuration fields cannot silently expand responses.
fn bounded<T: Serialize>(value: T) -> Result<Json<T>> {
    const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
    if serde_json::to_vec(&value).map_err(|_| corrupt())?.len() > MAX_RESPONSE_BYTES {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider_response_too_large",
            "Provider response exceeds the size limit",
        ));
    }
    Ok(Json(value))
}
async fn finish<T>(tx: libsql::Transaction, outcome: Result<T>) -> Result<T> {
    match outcome {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_wire_and_domain_validation_are_closed() {
        let input = serde_json::json!({"revision":1,"name":"Indexer","enabled":true,"priority":1,"settings":{"implementation":"newznab","endpoint":"https://example.invalid/api","tv":null,"movies":{"categories":[2147483647]}}});
        let update: ProviderUpdate = serde_json::from_value(input.clone()).unwrap();
        validate(&update.config).unwrap();
        assert!(matches!(update.config.credentials, Change::Missing));
        for (field, value) in [
            ("credentials", serde_json::Value::Null),
            (
                "credentials",
                serde_json::json!({"kind":"api_key","api_key":"valid"}),
            ),
        ] {
            let mut value_input = input.clone();
            value_input[field] = value;
            let parsed: ProviderUpdate = serde_json::from_value(value_input).unwrap();
            validate(&parsed.config).unwrap();
        }
        let mut unknown = input.clone();
        unknown["ignored"] = true.into();
        assert!(serde_json::from_value::<ProviderUpdate>(unknown).is_err());
        let mut absent = input.clone();
        absent["settings"].as_object_mut().unwrap().remove("tv");
        assert!(
            serde_json::from_value::<ProviderUpdate>(absent).is_err(),
            "Generated required-nullable scopes match actual deserialization"
        );
        for endpoint in [
            " https://example.invalid/api",
            "https://example.invalid/api ",
            r"https://example.invalid\api",
            "https://key@example.invalid/api",
            "https://example.invalid/api?apikey=secret",
            "https://example.invalid/api#secret",
        ] {
            let mut invalid = input.clone();
            invalid["settings"]["endpoint"] = endpoint.into();
            assert!(
                validate(
                    &serde_json::from_value::<ProviderUpdate>(invalid)
                        .unwrap()
                        .config
                )
                .is_err()
            );
        }
        // qBittorrent allows nested categories, but not malformed separators (both reference validators).
        for (category, valid) in [
            ("tv/anime", true),
            ("/tv", false),
            ("tv/", false),
            ("tv//anime", false),
            (r"tv\anime", false),
        ] {
            let settings = ProviderSettings::Qbittorrent {
                endpoint: "https://example.invalid".into(),
                tv: Some(DownloadScope {
                    category: category.into(),
                    imported_category: None,
                    recent_priority: 0,
                    older_priority: 1,
                }),
                movies: None,
            };
            assert_eq!(settings.validate().is_ok(), valid);
        }
        // Upstream category IDs are signed integers, not an invented six-digit ceiling.
        let mut invalid = input;
        invalid["settings"]["movies"]["categories"] = serde_json::json!([2147483648u32]);
        assert!(
            validate(
                &serde_json::from_value::<ProviderUpdate>(invalid)
                    .unwrap()
                    .config
            )
            .is_err()
        );
    }
}
