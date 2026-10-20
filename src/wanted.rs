//! Native Wanted lists (missing and quality cutoff unmet) for TV episodes and library movies.
//! GET reads count, page and diagnostics from one read transaction with one captured clock; it
//! performs no writes, filesystem checks or provider requests. File presence is the active DB
//! association (retired replacement files do not count). Wanted searches admit ordinary durable
//! user-search commands through `commands::search::admit_wanted`.
use crate::{
    api::{ApiErrorEnvelope, MediaDomain},
    commands::{self, CommandPriority, search::MAX_WANTED_TARGETS},
    db::{Database, MediaTarget},
    providers::{ClientSelection, RefreshClient},
    quality_profiles::{ThresholdBasis, quality_threshold},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, RawQuery, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use libsql::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const DEFAULT_LIMIT: u16 = 50;
const MAX_LIMIT: u16 = 100;
const MAX_OFFSET: u32 = 2_147_483_647;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_PROFILES: usize = 1000;
/// Rows a movie availability filter may evaluate in Rust before refusing explicitly.
const MAX_AVAILABILITY_SCAN: usize = 10_000;
const MAX_SAFE_ID: i64 = 9_007_199_254_740_991;
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorEnvelope::new(self.1, self.2))).into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(error: libsql::Error) -> Self {
        eprintln!(
            "event=wanted_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        unavailable()
    }
}
fn invalid(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_wanted_query", message)
}
fn unavailable() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "wanted_unavailable",
        "Wanted storage is unavailable; nothing was changed",
    )
}
fn resource_limit() -> Error {
    Error(
        StatusCode::PAYLOAD_TOO_LARGE,
        "wanted_resource_limit",
        "Wanted request exceeds a documented resource bound; narrow the filter or page",
    )
}
fn state_invalid() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "wanted_state_invalid",
        "Persisted Wanted policy is invalid",
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedKind {
    Missing,
    Cutoff,
}
/// Monitoring is a partition: TV `monitored` requires both episode and series monitoring and
/// `unmonitored` is its complement; movies compare the movie flag. Season flags are reported,
/// not filtered, matching the reference list (search eligibility is a separate policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedMonitored {
    Monitored,
    Unmonitored,
    All,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedSort {
    AirDate,
    SeriesTitle,
    Title,
    Year,
    InCinemas,
    DigitalRelease,
    PhysicalRelease,
    Quality,
    /// Effective last search (later of native and imported). Never-searched rows order as
    /// older than every timestamp: first ascending, last descending.
    LastSearch,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedDirection {
    Asc,
    Desc,
}
/// Optional movie-missing filter over the existing background availability rule. The default
/// (`all`) keeps unreleased and unavailable movies, as the reference list does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedAvailability {
    All,
    Available,
    Unavailable,
}
/// Documentation of the strictly validated URL query. Duplicate, unknown, empty or
/// domain-inapplicable keys are rejected with `invalid_wanted_query`.
#[allow(dead_code)]
#[derive(ts_rs::TS)]
#[ts(optional_fields)]
pub struct WantedQuery {
    limit: Option<u16>,
    offset: Option<u32>,
    monitored: Option<WantedMonitored>,
    sort: Option<WantedSort>,
    direction: Option<WantedDirection>,
    availability: Option<WantedAvailability>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct WantedPage<T> {
    pub media_type: MediaDomain,
    pub kind: WantedKind,
    pub items: Vec<T>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
    pub sort: WantedSort,
    pub direction: WantedDirection,
    pub monitored: WantedMonitored,
    pub evaluated_at: i64,
    pub diagnostics: WantedDiagnostics,
}
/// Rows excluded because a required fact is unknown, under the same monitoring filter.
/// `null` marks a counter that does not apply to this domain/list.
#[derive(Serialize, ts_rs::TS)]
pub struct WantedDiagnostics {
    pub unknown_air_date: Option<i64>,
    pub unknown_series_runtime: Option<i64>,
    pub unknown_year: Option<i64>,
    pub unknown_cutoff: Option<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct WantedFile {
    pub id: i64,
    pub path: String,
    pub quality_id: Option<i64>,
    pub quality_name: Option<String>,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedCutoffBasis {
    ConfiguredCutoff,
    FirstAllowed,
}
#[derive(Serialize, ts_rs::TS)]
pub struct WantedCutoff {
    pub profile_id: i64,
    pub quality_rank: i64,
    pub threshold_rank: i64,
    pub basis: WantedCutoffBasis,
}
/// Which fact supplies `last_search_time`: a native search dispatch, or a value imported from
/// a Sonarr snapshot (TV only) that is later than any native dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedLastSearchSource {
    Native,
    Imported,
}
fn last_search_source(value: Option<String>) -> Result<Option<WantedLastSearchSource>> {
    match value.as_deref() {
        None => Ok(None),
        Some("native") => Ok(Some(WantedLastSearchSource::Native)),
        Some("imported") => Ok(Some(WantedLastSearchSource::Imported)),
        Some(_) => Err(state_invalid()),
    }
}
#[derive(Serialize, ts_rs::TS)]
pub struct TvWantedItem {
    pub target: MediaTarget,
    pub series_id: i64,
    pub series_title: String,
    pub series_monitored: bool,
    pub series_type: Option<crate::library::SeriesType>,
    pub season: i64,
    pub season_monitored: Option<bool>,
    pub number: i64,
    pub absolute_episode_number: Option<i64>,
    pub scene_season_number: Option<i64>,
    pub scene_episode_number: Option<i64>,
    pub scene_absolute_episode_number: Option<i64>,
    pub title: String,
    pub monitored: bool,
    pub effective_monitored: bool,
    pub air_date: Option<String>,
    pub air_date_utc: Option<String>,
    // Air time plus series runtime, Unix seconds; `null` when either fact is unknown.
    pub aired_end_at: Option<i64>,
    // Effective last search, RFC 3339 UTC seconds: the later of the native dispatch fact and
    // an imported snapshot value; `null` when never searched (or the import is unparseable).
    pub last_search_time: Option<String>,
    pub last_search_source: Option<WantedLastSearchSource>,
    pub profile_id: Option<i64>,
    pub file: Option<WantedFile>,
    pub cutoff: Option<WantedCutoff>,
}
/// `ungated` means no movie availability policy is persisted, so background decisions apply
/// no availability veto. User-invoked searches bypass availability in every case.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WantedMovieAvailability {
    Available,
    Unavailable,
    Ungated,
}
#[derive(Serialize, ts_rs::TS)]
pub struct MovieWantedItem {
    pub target: MediaTarget,
    pub metadata_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub monitored: bool,
    pub status: Option<String>,
    pub minimum_availability: Option<crate::library::Availability>,
    pub in_cinemas: Option<String>,
    pub digital_release: Option<String>,
    pub physical_release: Option<String>,
    pub availability: WantedMovieAvailability,
    // Native last search, RFC 3339 UTC seconds; `null` when never searched natively.
    pub last_search_time: Option<String>,
    pub last_search_source: Option<WantedLastSearchSource>,
    pub profile_id: Option<i64>,
    pub file: Option<WantedFile>,
    pub cutoff: Option<WantedCutoff>,
}

#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WantedSelection {
    /// Explicit targets (episode IDs on TV routes, movie IDs on movie routes), searched even
    /// when unmonitored or already being searched. Duplicates are removed before bounds.
    Ids { ids: Vec<i64> },
    /// Every current member of the list under this filter, captured at admission. Targets
    /// with an active search command are skipped and counted.
    All {
        monitored: WantedMonitored,
        #[serde(default)]
        #[ts(optional)]
        availability: Option<WantedAvailability>,
    },
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct WantedSearchInput {
    pub request_id: Uuid,
    pub kind: WantedKind,
    pub selection: WantedSelection,
    pub indexer_id: Uuid,
    pub indexer_revision: i64,
    pub client_selection: ClientSelection,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct WantedSearchCommand {
    pub target: MediaTarget,
    pub command_id: Uuid,
}
#[derive(Serialize, ts_rs::TS)]
pub struct WantedSearchReceipt {
    pub request_id: Uuid,
    pub media_type: MediaDomain,
    pub kind: WantedKind,
    // True when an identical earlier request is read back; counts are then unknown (`null`).
    pub replayed: bool,
    pub matched: Option<u32>,
    pub skipped_active: Option<u32>,
    pub admitted: u32,
    pub commands: Vec<WantedSearchCommand>,
}

#[derive(Clone)]
struct Context {
    db: Arc<Database>,
    client: RefreshClient,
}
pub fn router(db: Arc<Database>, client: RefreshClient) -> Router {
    use MediaDomain::{Movies, Tv};
    use WantedKind::{Cutoff, Missing};
    Router::new()
        .route(
            "/api/v1/tv/wanted/missing",
            get(|s, q| list(s, q, Tv, Missing)),
        )
        .route(
            "/api/v1/tv/wanted/cutoff",
            get(|s, q| list(s, q, Tv, Cutoff)),
        )
        .route(
            "/api/v1/movies/wanted/missing",
            get(|s, q| list(s, q, Movies, Missing)),
        )
        .route(
            "/api/v1/movies/wanted/cutoff",
            get(|s, q| list(s, q, Movies, Cutoff)),
        )
        .route(
            "/api/v1/tv/wanted/search",
            post(|s, q, b| search(s, q, b, Tv)),
        )
        .route(
            "/api/v1/movies/wanted/search",
            post(|s, q, b| search(s, q, b, Movies)),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(Context { db, client })
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(REQUEST_DEADLINE, next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "wanted_unavailable",
            "Wanted request exceeded its deadline; no partial admission committed",
        )
        .into_response(),
    }
}

/// Validated list filter shared by GET pages and search-all capture.
#[derive(Clone, Copy)]
struct Filter {
    media: MediaDomain,
    kind: WantedKind,
    monitored: WantedMonitored,
    availability: WantedAvailability,
}
struct ListQuery {
    filter: Filter,
    limit: u16,
    offset: u32,
    sort: WantedSort,
    direction: WantedDirection,
}
fn digits<T: std::str::FromStr>(value: &str, max_len: usize) -> Option<T> {
    (!value.is_empty() && value.len() <= max_len && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}
fn parse_query(raw: Option<&str>, media: MediaDomain, kind: WantedKind) -> Result<ListQuery> {
    let raw = raw.unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return Err(invalid("Wanted query exceeds 4096 bytes"));
    }
    let mut query = ListQuery {
        filter: Filter {
            media,
            kind,
            monitored: WantedMonitored::Monitored,
            availability: WantedAvailability::All,
        },
        limit: DEFAULT_LIMIT,
        offset: 0,
        sort: match media {
            MediaDomain::Tv => WantedSort::AirDate,
            MediaDomain::Movies => WantedSort::Title,
        },
        direction: WantedDirection::Asc,
    };
    let mut seen = BTreeSet::new();
    if raw.is_empty() {
        return Ok(query);
    }
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if key.is_empty() || value.is_empty() {
            return Err(invalid("Wanted query keys and values must be non-empty"));
        }
        if !seen.insert(key.to_string()) {
            return Err(invalid("Wanted query keys must not repeat"));
        }
        match key.as_ref() {
            "limit" => {
                query.limit = digits::<u16>(&value, 3)
                    .filter(|v| (1..=MAX_LIMIT).contains(v))
                    .ok_or_else(|| invalid("limit must be an integer from 1 to 100"))?
            }
            "offset" => {
                query.offset = digits::<u32>(&value, 10)
                    .filter(|v| *v <= MAX_OFFSET)
                    .ok_or_else(|| invalid("offset must be an integer from 0 to 2147483647"))?
            }
            "monitored" => {
                query.filter.monitored = match value.as_ref() {
                    "monitored" => WantedMonitored::Monitored,
                    "unmonitored" => WantedMonitored::Unmonitored,
                    "all" => WantedMonitored::All,
                    _ => return Err(invalid("monitored must be monitored, unmonitored or all")),
                }
            }
            "direction" => {
                query.direction = match value.as_ref() {
                    "asc" => WantedDirection::Asc,
                    "desc" => WantedDirection::Desc,
                    _ => return Err(invalid("direction must be asc or desc")),
                }
            }
            "sort" => {
                query.sort = match (media, kind, value.as_ref()) {
                    (MediaDomain::Tv, _, "air_date") => WantedSort::AirDate,
                    (MediaDomain::Tv, _, "series_title") => WantedSort::SeriesTitle,
                    (MediaDomain::Movies, _, "title") => WantedSort::Title,
                    (MediaDomain::Movies, _, "year") => WantedSort::Year,
                    (MediaDomain::Movies, _, "in_cinemas") => WantedSort::InCinemas,
                    (MediaDomain::Movies, _, "digital_release") => WantedSort::DigitalRelease,
                    (MediaDomain::Movies, _, "physical_release") => WantedSort::PhysicalRelease,
                    (_, WantedKind::Cutoff, "quality") => WantedSort::Quality,
                    (_, _, "last_search") => WantedSort::LastSearch,
                    _ => return Err(invalid("sort is not supported for this Wanted list")),
                }
            }
            "availability" if media == MediaDomain::Movies && kind == WantedKind::Missing => {
                query.filter.availability = parse_availability(&value)?
            }
            _ => return Err(invalid("Unknown or inapplicable Wanted query key")),
        }
    }
    Ok(query)
}
fn parse_availability(value: &str) -> Result<WantedAvailability> {
    match value {
        "all" => Ok(WantedAvailability::All),
        "available" => Ok(WantedAvailability::Available),
        "unavailable" => Ok(WantedAvailability::Unavailable),
        _ => Err(invalid(
            "availability must be all, available or unavailable",
        )),
    }
}

fn clock() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .filter(|v| *v < MAX_SAFE_ID)
        .ok_or(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wanted_clock_error",
            "System clock is outside the supported range",
        ))
}
fn domain_name(media: MediaDomain) -> &'static str {
    match media {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
/// Per-profile quality ranks as an inline integer CTE. Only integers read from storage are
/// formatted into SQL. Profiles whose cutoff cannot be resolved contribute no rows, so their
/// files join no rank and are reported as unknown cutoff.
async fn ranks_cte(c: &Connection, media: MediaDomain, kind: WantedKind) -> Result<String> {
    const EMPTY: &str = "WITH ranks(profile_id,quality_id,rank,threshold,basis) AS (SELECT NULL,NULL,NULL,NULL,NULL WHERE 0)";
    if kind == WantedKind::Missing {
        return Ok(EMPTY.into());
    }
    let mut rows = c
        .query(
            "SELECT id FROM quality_profiles WHERE media_type=? ORDER BY id LIMIT ?",
            libsql::params![domain_name(media), MAX_PROFILES as i64 + 1],
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<i64>(0)?);
    }
    drop(rows);
    if ids.len() > MAX_PROFILES {
        return Err(resource_limit());
    }
    let mut values = Vec::new();
    for id in ids {
        let profile = crate::quality_profiles::fetch(c, domain_name(media), id)
            .await
            .map_err(|error| {
                if error.1 == "database_error" {
                    unavailable()
                } else {
                    state_invalid()
                }
            })?;
        if let Some(threshold) = quality_threshold(&profile) {
            let basis = match threshold.basis {
                ThresholdBasis::ConfiguredCutoff => 0,
                ThresholdBasis::FirstAllowed => 1,
            };
            for (quality, rank) in &threshold.ranks {
                values.push(format!(
                    "({id},{quality},{rank},{},{basis})",
                    threshold.threshold
                ));
            }
        }
    }
    Ok(if values.is_empty() {
        EMPTY.into()
    } else {
        format!(
            "WITH ranks(profile_id,quality_id,rank,threshold,basis) AS (VALUES {})",
            values.join(",")
        )
    })
}
const TV_FROM: &str = " FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN seasons se ON se.series_id=e.series_id AND se.number=e.season LEFT JOIN library_settings l ON l.series_id=s.id AND l.media_type='tv' LEFT JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id AND {active} LEFT JOIN file_metadata fm ON fm.media_type='tv' AND fm.episode_file_id=f.id LEFT JOIN ranks k ON k.profile_id=l.quality_profile_id AND k.quality_id=fm.quality_id LEFT JOIN quality_definitions q ON q.media_type='tv' AND q.quality_id=fm.quality_id";
const MOVIE_FROM: &str = " FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN library_settings l ON l.movie_id=m.id AND l.media_type='movies' LEFT JOIN movie_files f ON f.movie_id=m.id AND {active} LEFT JOIN file_metadata fm ON fm.media_type='movies' AND fm.movie_file_id=f.id LEFT JOIN ranks k ON k.profile_id=l.quality_profile_id AND k.quality_id=fm.quality_id LEFT JOIN quality_definitions q ON q.media_type='movies' AND q.quality_id=fm.quality_id";
const TV_AIRED_END: &str = "(unixepoch(e.air_date_utc)+s.runtime*60)";
fn from(media: MediaDomain) -> String {
    let (template, name) = match media {
        MediaDomain::Tv => (TV_FROM, "tv"),
        MediaDomain::Movies => (MOVIE_FROM, "movies"),
    };
    template.replace("{active}", crate::media_files::active_file_sql(name))
}
fn monitored_sql(filter: &Filter) -> &'static str {
    match (filter.media, filter.monitored) {
        (_, WantedMonitored::All) => "1",
        (MediaDomain::Tv, WantedMonitored::Monitored) => "(e.monitored=1 AND s.monitored=1)",
        (MediaDomain::Tv, WantedMonitored::Unmonitored) => "NOT (e.monitored=1 AND s.monitored=1)",
        (MediaDomain::Movies, WantedMonitored::Monitored) => "m.monitored=1",
        (MediaDomain::Movies, WantedMonitored::Unmonitored) => "m.monitored=0",
    }
}
/// Membership predicate. TV missing requires known UTC air time plus known series runtime at
/// or before the captured clock and includes specials; TV cutoff excludes specials and has no
/// air-time gate. Movie missing requires a positive year and no release/availability gate.
fn membership_sql(filter: &Filter, now: i64) -> String {
    match (filter.media, filter.kind) {
        (MediaDomain::Tv, WantedKind::Missing) => {
            format!("f.id IS NULL AND {TV_AIRED_END} IS NOT NULL AND {TV_AIRED_END}<={now}")
        }
        (MediaDomain::Tv, WantedKind::Cutoff) => {
            "f.id IS NOT NULL AND e.season>=1 AND k.rank<k.threshold".into()
        }
        (MediaDomain::Movies, WantedKind::Missing) => "f.id IS NULL AND d.year>0".into(),
        (MediaDomain::Movies, WantedKind::Cutoff) => {
            "f.id IS NOT NULL AND k.rank<k.threshold".into()
        }
    }
}
/// Effective last-search instant in Unix seconds. TV takes the later of the native dispatch
/// fact and the imported snapshot text; movies have only the native fact.
const TV_LAST_SEARCH: &str = "coalesce(max(e.native_last_search_at,unixepoch(e.last_search_time)),e.native_last_search_at,unixepoch(e.last_search_time))";
const MOVIE_LAST_SEARCH: &str = "m.native_last_search_at";
fn last_search_key(media: MediaDomain) -> (&'static str, &'static str) {
    match media {
        MediaDomain::Tv => (TV_LAST_SEARCH, "e.id"),
        MediaDomain::Movies => (MOVIE_LAST_SEARCH, "m.id"),
    }
}
/// Oldest-searched-first: never searched first, then oldest instant, ties by typed ID.
fn oldest_first_sql(media: MediaDomain) -> String {
    let (key, id) = last_search_key(media);
    format!(" ORDER BY ({key}) IS NOT NULL,{key} ASC,{id}")
}
fn order_sql(media: MediaDomain, sort: WantedSort, direction: WantedDirection) -> String {
    let dir = match direction {
        WantedDirection::Asc => "ASC",
        WantedDirection::Desc => "DESC",
    };
    if sort == WantedSort::LastSearch {
        // Never searched counts as older than every instant (not as an unknown value), so it
        // leads ascending (oldest-searched-first) and trails descending.
        let (key, id) = last_search_key(media);
        return match direction {
            WantedDirection::Asc => oldest_first_sql(media),
            WantedDirection::Desc => format!(" ORDER BY ({key}) IS NULL,{key} DESC,{id}"),
        };
    }
    let (key, ties) = match (media, sort) {
        (MediaDomain::Tv, WantedSort::SeriesTitle) => {
            ("lower(s.title)", "s.id,e.season,e.number,e.id")
        }
        (MediaDomain::Tv, WantedSort::Quality) => ("k.rank", "e.id"),
        (MediaDomain::Tv, _) => ("unixepoch(e.air_date_utc)", "e.id"),
        (MediaDomain::Movies, WantedSort::Year) => ("d.year", "m.id"),
        (MediaDomain::Movies, WantedSort::InCinemas) => ("unixepoch(d.in_cinemas)", "m.id"),
        (MediaDomain::Movies, WantedSort::DigitalRelease) => {
            ("unixepoch(d.digital_release)", "m.id")
        }
        (MediaDomain::Movies, WantedSort::PhysicalRelease) => {
            ("unixepoch(d.physical_release)", "m.id")
        }
        (MediaDomain::Movies, WantedSort::Quality) => ("k.rank", "m.id"),
        (MediaDomain::Movies, _) => ("lower(d.title)", "m.id"),
    };
    // Unknown sort values stay last in either direction; ties are stable ascending typed IDs.
    format!(" ORDER BY ({key}) IS NULL,{key} {dir},{ties}")
}
const TV_COLUMNS: &str = "SELECT e.id,e.series_id,s.title,s.monitored,l.series_type,e.season,se.monitored,e.number,e.absolute_episode_number,e.scene_season_number,e.scene_episode_number,e.scene_absolute_episode_number,e.title,e.monitored,e.air_date,e.air_date_utc,(unixepoch(e.air_date_utc)+s.runtime*60),strftime('%Y-%m-%dT%H:%M:%SZ',coalesce(max(e.native_last_search_at,unixepoch(e.last_search_time)),e.native_last_search_at,unixepoch(e.last_search_time)),'unixepoch'),l.quality_profile_id,f.id,f.path,fm.quality_id,q.title,k.rank,k.threshold,k.basis,CASE WHEN e.native_last_search_at IS NOT NULL AND (unixepoch(e.last_search_time) IS NULL OR e.native_last_search_at>=unixepoch(e.last_search_time)) THEN 'native' WHEN unixepoch(e.last_search_time) IS NOT NULL THEN 'imported' END";
const MOVIE_COLUMNS: &str = "SELECT m.id,d.id,d.title,d.year,d.tmdb_id,d.imdb_id,m.monitored,d.status,l.minimum_availability,d.in_cinemas,d.digital_release,d.physical_release,l.quality_profile_id,f.id,f.path,fm.quality_id,q.title,k.rank,k.threshold,k.basis,strftime('%Y-%m-%dT%H:%M:%SZ',m.native_last_search_at,'unixepoch'),CASE WHEN m.native_last_search_at IS NOT NULL THEN 'native' END";

fn file(row: &libsql::Row, first: i32) -> Result<Option<WantedFile>> {
    let id = row.get::<Option<i64>>(first)?;
    let path = row.get::<Option<String>>(first + 1)?;
    let quality_id = row.get::<Option<i64>>(first + 2)?;
    let quality_name = row.get::<Option<String>>(first + 3)?;
    Ok(id.zip(path).map(|(id, path)| WantedFile {
        id,
        path,
        quality_id,
        quality_name,
    }))
}
fn cutoff(
    row: &libsql::Row,
    kind: WantedKind,
    profile: Option<i64>,
    first: i32,
) -> Result<Option<WantedCutoff>> {
    if kind != WantedKind::Cutoff {
        return Ok(None);
    }
    let (Some(profile_id), Some(quality_rank), Some(threshold_rank), Some(basis)) = (
        profile,
        row.get::<Option<i64>>(first)?,
        row.get::<Option<i64>>(first + 1)?,
        row.get::<Option<i64>>(first + 2)?,
    ) else {
        return Err(state_invalid());
    };
    Ok(Some(WantedCutoff {
        profile_id,
        quality_rank,
        threshold_rank,
        basis: if basis == 1 {
            WantedCutoffBasis::FirstAllowed
        } else {
            WantedCutoffBasis::ConfiguredCutoff
        },
    }))
}
fn tv_item(row: &libsql::Row, kind: WantedKind) -> Result<TvWantedItem> {
    let monitored = row.get::<i64>(13)? == 1;
    let series_monitored = row.get::<i64>(3)? == 1;
    let profile = row.get::<Option<i64>>(18)?;
    Ok(TvWantedItem {
        target: MediaTarget::Episode(row.get(0)?),
        series_id: row.get(1)?,
        series_title: row.get(2)?,
        series_monitored,
        series_type: match row.get::<Option<String>>(4)?.as_deref() {
            None => None,
            Some("standard") => Some(crate::library::SeriesType::Standard),
            Some("daily") => Some(crate::library::SeriesType::Daily),
            Some("anime") => Some(crate::library::SeriesType::Anime),
            Some(_) => return Err(state_invalid()),
        },
        season: row.get(5)?,
        season_monitored: row.get::<Option<i64>>(6)?.map(|v| v == 1),
        number: row.get(7)?,
        absolute_episode_number: row.get(8)?,
        scene_season_number: row.get(9)?,
        scene_episode_number: row.get(10)?,
        scene_absolute_episode_number: row.get(11)?,
        title: row.get(12)?,
        monitored,
        effective_monitored: monitored && series_monitored,
        air_date: row.get(14)?,
        air_date_utc: row.get(15)?,
        aired_end_at: row.get(16)?,
        last_search_time: row.get(17)?,
        last_search_source: last_search_source(row.get(26)?)?,
        profile_id: profile,
        file: file(row, 19)?,
        cutoff: cutoff(row, kind, profile, 23)?,
    })
}
fn movie_item(
    row: &libsql::Row,
    kind: WantedKind,
    now: i64,
    delay: Option<i64>,
) -> Result<MovieWantedItem> {
    let minimum = row.get::<Option<String>>(8)?;
    let status = row.get::<Option<String>>(7)?;
    let cinema = row.get::<Option<String>>(9)?;
    let digital = row.get::<Option<String>>(10)?;
    let physical = row.get::<Option<String>>(11)?;
    let availability = match delay {
        None => WantedMovieAvailability::Ungated,
        Some(days) => {
            if crate::search::movie_available(
                &crate::search::MovieReleaseFacts {
                    minimum: minimum.as_deref(),
                    status: status.as_deref(),
                    cinema: cinema.as_deref(),
                    digital: digital.as_deref(),
                    physical: physical.as_deref(),
                },
                now,
                days,
            ) {
                WantedMovieAvailability::Available
            } else {
                WantedMovieAvailability::Unavailable
            }
        }
    };
    let profile = row.get::<Option<i64>>(12)?;
    Ok(MovieWantedItem {
        target: MediaTarget::Movie(row.get(0)?),
        metadata_id: row.get(1)?,
        title: row.get(2)?,
        year: row.get(3)?,
        tmdb_id: row.get(4)?,
        imdb_id: row.get(5)?,
        monitored: row.get::<i64>(6)? == 1,
        status,
        minimum_availability: match minimum.as_deref() {
            None => None,
            Some("tba") => Some(crate::library::Availability::Tba),
            Some("announced") => Some(crate::library::Availability::Announced),
            Some("in_cinemas") => Some(crate::library::Availability::InCinemas),
            Some("released") => Some(crate::library::Availability::Released),
            Some(_) => return Err(state_invalid()),
        },
        in_cinemas: cinema,
        digital_release: digital,
        physical_release: physical,
        availability,
        last_search_time: row.get(20)?,
        last_search_source: last_search_source(row.get(21)?)?,
        profile_id: profile,
        file: file(row, 13)?,
        cutoff: cutoff(row, kind, profile, 17)?,
    })
}
fn keeps(filter: WantedAvailability, value: WantedMovieAvailability) -> bool {
    match filter {
        WantedAvailability::All => true,
        WantedAvailability::Available => value != WantedMovieAvailability::Unavailable,
        WantedAvailability::Unavailable => value == WantedMovieAvailability::Unavailable,
    }
}
async fn single(c: &Connection, sql: &str) -> Result<Vec<i64>> {
    let row = c
        .query(sql, ())
        .await?
        .next()
        .await?
        .ok_or_else(unavailable)?;
    let mut values = Vec::new();
    for index in 0..row.column_count() {
        values.push(row.get::<Option<i64>>(index)?.unwrap_or(0));
    }
    Ok(values)
}
async fn diagnostics(c: &Connection, cte: &str, filter: &Filter) -> Result<WantedDiagnostics> {
    let from = from(filter.media);
    let monitored = monitored_sql(filter);
    let mut out = WantedDiagnostics {
        unknown_air_date: None,
        unknown_series_runtime: None,
        unknown_year: None,
        unknown_cutoff: None,
    };
    match (filter.media, filter.kind) {
        (MediaDomain::Tv, WantedKind::Missing) => {
            let v = single(c, &format!("{cte} SELECT sum(unixepoch(e.air_date_utc) IS NULL),sum(unixepoch(e.air_date_utc) IS NOT NULL AND s.runtime IS NULL){from} WHERE {monitored} AND f.id IS NULL")).await?;
            out.unknown_air_date = Some(v[0]);
            out.unknown_series_runtime = Some(v[1]);
        }
        (MediaDomain::Tv, WantedKind::Cutoff) => {
            out.unknown_cutoff = Some(single(c, &format!("{cte} SELECT count(*){from} WHERE {monitored} AND f.id IS NOT NULL AND e.season>=1 AND k.profile_id IS NULL")).await?[0]);
        }
        (MediaDomain::Movies, WantedKind::Missing) => {
            out.unknown_year = Some(single(c, &format!("{cte} SELECT count(*){from} WHERE {monitored} AND f.id IS NULL AND (d.year IS NULL OR d.year<=0)")).await?[0]);
        }
        (MediaDomain::Movies, WantedKind::Cutoff) => {
            out.unknown_cutoff = Some(single(c, &format!("{cte} SELECT count(*){from} WHERE {monitored} AND f.id IS NOT NULL AND k.profile_id IS NULL")).await?[0]);
        }
    }
    Ok(out)
}
/// Movie rows with the availability filter applied in Rust through the shared background rule.
/// Bounded: more than `MAX_AVAILABILITY_SCAN` candidate rows is an explicit refusal.
async fn filtered_movies(
    c: &Connection,
    sql: &str,
    filter: &Filter,
    now: i64,
    delay: Option<i64>,
) -> Result<Vec<MovieWantedItem>> {
    let mut rows = c
        .query(&format!("{sql} LIMIT {}", MAX_AVAILABILITY_SCAN + 1), ())
        .await?;
    let mut items = Vec::new();
    let mut scanned = 0usize;
    while let Some(row) = rows.next().await? {
        scanned += 1;
        if scanned > MAX_AVAILABILITY_SCAN {
            return Err(resource_limit());
        }
        let item = movie_item(&row, filter.kind, now, delay)?;
        if keeps(filter.availability, item.availability) {
            items.push(item);
        }
    }
    Ok(items)
}
fn page<T>(
    query: &ListQuery,
    now: i64,
    diagnostics: WantedDiagnostics,
    total: i64,
    items: Vec<T>,
) -> WantedPage<T> {
    WantedPage {
        media_type: query.filter.media,
        kind: query.filter.kind,
        items,
        total,
        limit: query.limit,
        offset: query.offset,
        sort: query.sort,
        direction: query.direction,
        monitored: query.filter.monitored,
        evaluated_at: now,
        diagnostics,
    }
}
async fn read_page(c: &Connection, query: &ListQuery, now: i64) -> Result<Response> {
    let filter = &query.filter;
    let cte = ranks_cte(c, filter.media, filter.kind).await?;
    let from = from(filter.media);
    let filter_sql = format!(
        "WHERE {} AND {}",
        monitored_sql(filter),
        membership_sql(filter, now)
    );
    let order = order_sql(filter.media, query.sort, query.direction);
    let diagnostics = diagnostics(c, &cte, filter).await?;
    let body = match filter.media {
        MediaDomain::Tv => {
            let total = single(c, &format!("{cte} SELECT count(*){from} {filter_sql}")).await?[0];
            let mut rows = c
                .query(
                    &format!(
                        "{cte} {TV_COLUMNS}{from} {filter_sql}{order} LIMIT {} OFFSET {}",
                        query.limit, query.offset
                    ),
                    (),
                )
                .await?;
            let mut items = Vec::new();
            while let Some(row) = rows.next().await? {
                items.push(tv_item(&row, filter.kind)?);
            }
            serde_json::to_vec(&page(query, now, diagnostics, total, items))
        }
        MediaDomain::Movies => {
            let delay = crate::search::movie_availability_delay(c)
                .await
                .map_err(|_| unavailable())?;
            let sql = format!("{cte} {MOVIE_COLUMNS}{from} {filter_sql}{order}");
            let (total, items) = if filter.availability == WantedAvailability::All {
                let total =
                    single(c, &format!("{cte} SELECT count(*){from} {filter_sql}")).await?[0];
                let mut rows = c
                    .query(
                        &format!("{sql} LIMIT {} OFFSET {}", query.limit, query.offset),
                        (),
                    )
                    .await?;
                let mut items = Vec::new();
                while let Some(row) = rows.next().await? {
                    items.push(movie_item(&row, filter.kind, now, delay)?);
                }
                (total, items)
            } else {
                let all = filtered_movies(c, &sql, filter, now, delay).await?;
                let total = all.len() as i64;
                let items = all
                    .into_iter()
                    .skip(query.offset as usize)
                    .take(usize::from(query.limit))
                    .collect();
                (total, items)
            };
            serde_json::to_vec(&page(query, now, diagnostics, total, items))
        }
    }
    .map_err(|_| state_invalid())?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(resource_limit());
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response())
}
async fn list(
    State(ctx): State<Context>,
    RawQuery(raw): RawQuery,
    media: MediaDomain,
    kind: WantedKind,
) -> Result<Response> {
    let query = parse_query(raw.as_deref(), media, kind)?;
    let now = clock()?;
    let c = ctx.db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = read_page(&tx, &query, now).await;
    // Read-only: nothing to commit; always release the snapshot.
    tx.rollback().await?;
    outcome
}

/// Every current list member under `filter`, oldest-searched-first (never searched first, ties
/// by typed ID), at most `MAX_WANTED_TARGETS + 1` so the admission layer can refuse an
/// over-capacity capture rather than truncate it. The order is total, so the read bound and the
/// admission/claim order are deterministic.
async fn capture(c: &Connection, filter: Filter, now: i64) -> Result<Vec<i64>> {
    let cte = ranks_cte(c, filter.media, filter.kind).await?;
    let from = from(filter.media);
    let filter_sql = format!(
        "WHERE {} AND {}",
        monitored_sql(&filter),
        membership_sql(&filter, now)
    );
    if filter.media == MediaDomain::Movies && filter.availability != WantedAvailability::All {
        let delay = crate::search::movie_availability_delay(c)
            .await
            .map_err(|_| unavailable())?;
        let sql = format!(
            "{cte} {MOVIE_COLUMNS}{from} {filter_sql}{}",
            oldest_first_sql(MediaDomain::Movies)
        );
        let items = filtered_movies(c, &sql, &filter, now, delay).await?;
        return Ok(items
            .into_iter()
            .take(MAX_WANTED_TARGETS + 1)
            .map(|item| match item.target {
                MediaTarget::Movie(id) | MediaTarget::Episode(id) => id,
            })
            .collect());
    }
    let id = match filter.media {
        MediaDomain::Tv => "e.id",
        MediaDomain::Movies => "m.id",
    };
    let mut rows = c
        .query(
            &format!(
                "{cte} SELECT {id}{from} {filter_sql}{} LIMIT {}",
                oldest_first_sql(filter.media),
                MAX_WANTED_TARGETS + 1
            ),
            (),
        )
        .await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<i64>(0)?);
    }
    Ok(ids)
}
/// Explicit selections are admitted oldest-searched-first too. Unknown IDs order as never
/// searched and still fail admission as a whole (`target_changed`).
async fn order_explicit(c: &Connection, media: MediaDomain, ids: &[i64]) -> Result<Vec<i64>> {
    let (join, order) = match media {
        MediaDomain::Tv => (
            "LEFT JOIN episodes e ON e.id=j.value",
            format!(" ORDER BY ({TV_LAST_SEARCH}) IS NOT NULL,{TV_LAST_SEARCH} ASC,j.value"),
        ),
        MediaDomain::Movies => (
            "LEFT JOIN movies m ON m.id=j.value",
            format!(" ORDER BY ({MOVIE_LAST_SEARCH}) IS NOT NULL,{MOVIE_LAST_SEARCH} ASC,j.value"),
        ),
    };
    let json = serde_json::to_string(ids).map_err(|_| state_invalid())?;
    let mut rows = c
        .query(
            &format!("SELECT j.value FROM json_each(?) j {join}{order}"),
            [json],
        )
        .await?;
    let mut ordered = Vec::with_capacity(ids.len());
    while let Some(row) = rows.next().await? {
        ordered.push(row.get::<i64>(0)?);
    }
    Ok(ordered)
}
fn command_error(code: &'static str) -> commands::Error {
    commands::Error(StatusCode::BAD_REQUEST, code)
}
async fn search(
    State(ctx): State<Context>,
    RawQuery(raw): RawQuery,
    body: std::result::Result<Json<WantedSearchInput>, JsonRejection>,
    media: MediaDomain,
) -> std::result::Result<(StatusCode, Json<WantedSearchReceipt>), commands::Error> {
    if raw.is_some_and(|q| !q.is_empty()) {
        return Err(command_error("invalid_command_request"));
    }
    let input = body
        .map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                commands::Error(StatusCode::PAYLOAD_TOO_LARGE, "wanted_search_target_limit")
            } else {
                command_error("invalid_command_request")
            }
        })?
        .0;
    if !(1..=MAX_SAFE_ID).contains(&input.indexer_revision) {
        return Err(command_error("invalid_command_request"));
    }
    let (selection, skip_active, filter) = match &input.selection {
        WantedSelection::Ids { ids } => {
            let unique: BTreeSet<i64> = ids.iter().copied().collect();
            if unique.is_empty() || unique.iter().any(|id| !(1..=MAX_SAFE_ID).contains(id)) {
                return Err(command_error("invalid_command_request"));
            }
            if unique.len() > MAX_WANTED_TARGETS {
                return Err(commands::Error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "wanted_search_target_limit",
                ));
            }
            (
                serde_json::json!({"kind":"ids","ids":unique.iter().collect::<Vec<_>>()}),
                false,
                None,
            )
        }
        WantedSelection::All {
            monitored,
            availability,
        } => {
            if availability.is_some()
                && !(media == MediaDomain::Movies && input.kind == WantedKind::Missing)
            {
                return Err(command_error("invalid_command_request"));
            }
            let availability = availability.unwrap_or(WantedAvailability::All);
            (
                serde_json::json!({"kind":"all","monitored":monitored,"availability":availability}),
                true,
                Some(Filter {
                    media,
                    kind: input.kind,
                    monitored: *monitored,
                    availability,
                }),
            )
        }
    };
    // Canonical body: serde_json maps serialize with sorted keys, IDs are sorted and unique.
    let canonical = serde_json::json!({
        "version": 1,
        "media_type": media,
        "kind": input.kind,
        "selection": selection,
        "indexer_id": input.indexer_id,
        "indexer_revision": input.indexer_revision,
        "client_selection": input.client_selection,
        "priority": input.priority,
    });
    let mut digest = [0u8; 32];
    digest.copy_from_slice(
        ring::digest::digest(&ring::digest::SHA256, canonical.to_string().as_bytes()).as_ref(),
    );
    let explicit: Vec<i64> = match &input.selection {
        WantedSelection::Ids { ids } => ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        WantedSelection::All { .. } => Vec::new(),
    };
    let source = commands::rss::RssTargetInput {
        media_type: media,
        indexer_id: input.indexer_id,
        indexer_revision: input.indexer_revision,
        client_selection: Some(input.client_selection),
        client_id: None,
        client_revision: None,
    };
    let admission = commands::search::admit_wanted(
        &ctx.db,
        &ctx.client,
        input.request_id,
        digest,
        source,
        input.priority,
        skip_active,
        async move |c: &Connection, now: i64| match filter {
            None => order_explicit(c, media, &explicit)
                .await
                .map_err(|error| commands::Error(error.0, error.1)),
            Some(filter) => capture(c, filter, now)
                .await
                .map_err(|error| commands::Error(error.0, error.1)),
        },
    )
    .await?;
    let commands = admission
        .children
        .into_iter()
        .map(|child| WantedSearchCommand {
            target: child.target,
            command_id: child.command_id,
        })
        .collect::<Vec<_>>();
    Ok((
        StatusCode::ACCEPTED,
        Json(WantedSearchReceipt {
            request_id: input.request_id,
            media_type: media,
            kind: input.kind,
            replayed: admission.replayed,
            matched: admission.matched,
            skipped_active: admission.skipped_active,
            admitted: commands.len() as u32,
            commands,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(raw: &str, media: MediaDomain, kind: WantedKind) -> Result<ListQuery> {
        parse_query(Some(raw), media, kind)
    }
    #[test]
    fn query_defaults_are_bounded_and_domain_specific() {
        let tv = parse("", MediaDomain::Tv, WantedKind::Missing).unwrap();
        assert_eq!((tv.limit, tv.offset, tv.sort), (50, 0, WantedSort::AirDate));
        assert_eq!(tv.direction, WantedDirection::Asc);
        assert_eq!(tv.filter.monitored, WantedMonitored::Monitored);
        let movie = parse("", MediaDomain::Movies, WantedKind::Cutoff).unwrap();
        assert_eq!(movie.sort, WantedSort::Title);
    }
    #[test]
    fn query_rejects_duplicates_unknown_empty_and_inapplicable_keys() {
        for (raw, media, kind) in [
            ("limit=1&limit=2", MediaDomain::Tv, WantedKind::Missing),
            ("limit=0", MediaDomain::Tv, WantedKind::Missing),
            ("limit=101", MediaDomain::Tv, WantedKind::Missing),
            ("limit=+5", MediaDomain::Tv, WantedKind::Missing),
            ("offset=2147483648", MediaDomain::Tv, WantedKind::Missing),
            ("offset=", MediaDomain::Tv, WantedKind::Missing),
            ("page=1", MediaDomain::Tv, WantedKind::Missing),
            ("sort=title", MediaDomain::Tv, WantedKind::Missing),
            ("sort=air_date", MediaDomain::Movies, WantedKind::Missing),
            ("sort=quality", MediaDomain::Movies, WantedKind::Missing),
            (
                "availability=available",
                MediaDomain::Tv,
                WantedKind::Missing,
            ),
            (
                "availability=available",
                MediaDomain::Movies,
                WantedKind::Cutoff,
            ),
            ("monitored=true", MediaDomain::Movies, WantedKind::Missing),
            ("direction=descending", MediaDomain::Tv, WantedKind::Cutoff),
            ("=x", MediaDomain::Tv, WantedKind::Missing),
        ] {
            let error = parse(raw, media, kind)
                .err()
                .unwrap_or_else(|| panic!("{raw}"));
            assert_eq!(
                (error.0, error.1),
                (StatusCode::BAD_REQUEST, "invalid_wanted_query")
            );
        }
        let long = format!("monitored=all&{}", "x".repeat(MAX_QUERY_BYTES));
        assert!(parse(&long, MediaDomain::Tv, WantedKind::Missing).is_err());
    }
    #[test]
    fn query_accepts_documented_keys() {
        let q = parse(
            "limit=100&offset=2147483647&monitored=unmonitored&sort=quality&direction=desc",
            MediaDomain::Tv,
            WantedKind::Cutoff,
        )
        .unwrap();
        assert_eq!(
            (q.limit, q.offset, q.sort),
            (100, MAX_OFFSET, WantedSort::Quality)
        );
        assert_eq!(q.filter.monitored, WantedMonitored::Unmonitored);
        let m = parse(
            "availability=unavailable&sort=digital_release",
            MediaDomain::Movies,
            WantedKind::Missing,
        )
        .unwrap();
        assert_eq!(m.filter.availability, WantedAvailability::Unavailable);
        assert_eq!(m.sort, WantedSort::DigitalRelease);
    }
    #[test]
    fn last_search_orders_never_searched_as_oldest_with_typed_ties() {
        let asc = order_sql(
            MediaDomain::Movies,
            WantedSort::LastSearch,
            WantedDirection::Asc,
        );
        assert_eq!(
            asc,
            " ORDER BY (m.native_last_search_at) IS NOT NULL,m.native_last_search_at ASC,m.id"
        );
        let desc = order_sql(
            MediaDomain::Movies,
            WantedSort::LastSearch,
            WantedDirection::Desc,
        );
        assert_eq!(
            desc,
            " ORDER BY (m.native_last_search_at) IS NULL,m.native_last_search_at DESC,m.id"
        );
        assert!(
            order_sql(
                MediaDomain::Tv,
                WantedSort::LastSearch,
                WantedDirection::Asc
            )
            .ends_with(" ASC,e.id")
        );
        for (media, kind) in [
            (MediaDomain::Tv, WantedKind::Missing),
            (MediaDomain::Tv, WantedKind::Cutoff),
            (MediaDomain::Movies, WantedKind::Missing),
            (MediaDomain::Movies, WantedKind::Cutoff),
        ] {
            assert_eq!(
                parse("sort=last_search", media, kind).unwrap().sort,
                WantedSort::LastSearch
            );
        }
    }
    #[test]
    fn order_keeps_nulls_last_and_stable_typed_ties() {
        let tv = order_sql(MediaDomain::Tv, WantedSort::AirDate, WantedDirection::Desc);
        assert_eq!(
            tv,
            " ORDER BY (unixepoch(e.air_date_utc)) IS NULL,unixepoch(e.air_date_utc) DESC,e.id"
        );
        assert!(
            order_sql(MediaDomain::Movies, WantedSort::Title, WantedDirection::Asc)
                .ends_with("lower(d.title) ASC,m.id")
        );
    }
}
