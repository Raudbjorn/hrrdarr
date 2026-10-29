//! Native Calendar for both domains: TV episode air times and library-movie release dates
//! (in cinemas, digital, physical) inside an explicit bounded window, plus an RFC 5545 feed
//! over the same selector. GET is read-only: one read transaction and one captured clock, no
//! writes, provider requests, filesystem checks or searches. The domain is part of the path
//! and every entry carries a typed `MediaTarget`, so equal numeric IDs never meet.
//!
//! Stored dates are selected with SQLite `unixepoch()` (which accepts both stored producer
//! encodings) and every selected value is re-validated strictly in Rust; values SQLite cannot
//! read, or reads differently, are reported as `invalid_dates` and never shown or guessed.
mod ical;

use crate::{
    api::{ApiErrorEnvelope, MediaDomain},
    db::{Database, MediaTarget},
};
use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Utc};
use libsql::{Connection, TransactionBehavior};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Longest window, in calendar dates (`end_date - first_date`).
pub const MAX_WINDOW_DAYS: i64 = 366;
/// Entries (movie releases count separately) one JSON response may hold.
pub const MAX_ENTRIES: usize = 2000;
/// Events one feed may hold.
pub const MAX_FEED_ENTRIES: usize = 10_000;
pub const MAX_FEED_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TAGS: usize = 64;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_TIMESTAMP_BYTES: usize = 40;
const MAX_FRACTION_DIGITS: usize = 9;
const MAX_SAFE_ID: i64 = 9_007_199_254_740_991;
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
const DEFAULT_PAST_DAYS: i64 = 7;
const DEFAULT_FUTURE_DAYS: i64 = 28;
const SECONDS_PER_MINUTE: i64 = 60;

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
            "event=calendar_storage_error kind={:?}",
            std::mem::discriminant(&error)
        );
        unavailable()
    }
}
fn invalid(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_calendar_query", message)
}
fn unavailable() -> Error {
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "calendar_unavailable",
        "Calendar storage is unavailable",
    )
}
fn resource_limit() -> Error {
    Error(
        StatusCode::PAYLOAD_TOO_LARGE,
        "calendar_resource_limit",
        "Calendar request exceeds a documented resource bound; narrow the window or filters",
    )
}
fn state_invalid() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "calendar_state_invalid",
        "Persisted calendar facts are invalid",
    )
}
fn feed_incomplete() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "calendar_state_invalid",
        "Some stored air or release dates are invalid; the feed is not published partially",
    )
}

/// Movie release kinds; each is a separate calendar entry, never merged with the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MovieReleaseType {
    InCinemas,
    Digital,
    Physical,
}
impl MovieReleaseType {
    const ALL: [Self; 3] = [Self::InCinemas, Self::Digital, Self::Physical];
    fn column(self) -> &'static str {
        match self {
            Self::InCinemas => "in_cinemas",
            Self::Digital => "digital_release",
            Self::Physical => "physical_release",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::InCinemas => "in_cinemas",
            Self::Digital => "digital",
            Self::Physical => "physical",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::InCinemas => "In cinemas",
            Self::Digital => "Digital release",
            Self::Physical => "Physical release",
        }
    }
    fn order(self) -> i64 {
        match self {
            Self::InCinemas => 0,
            Self::Digital => 1,
            Self::Physical => 2,
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == value)
    }
}
/// Which persisted runtime supplied a TV entry's duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CalendarRuntimeSource {
    Series,
    Episode,
}

/// Documentation of the strictly validated JSON query. `start`/`end` are `YYYY-MM-DD` dates or
/// whole-second RFC 3339 instants (percent-encode `+`). `tags` and `release_types` are
/// comma-separated. Unknown, repeated, empty or domain-inapplicable keys are rejected.
#[allow(dead_code)]
#[derive(ts_rs::TS)]
#[ts(optional_fields)]
pub struct CalendarQuery {
    start: String,
    end: String,
    unmonitored: Option<bool>,
    tags: Option<String>,
    specials: Option<bool>,
    release_types: Option<String>,
}
/// Feed query: the window is UTC dates `[today - past_days, today + future_days]`.
#[allow(dead_code)]
#[derive(ts_rs::TS)]
#[ts(optional_fields)]
pub struct CalendarFeedQuery {
    past_days: Option<u16>,
    future_days: Option<u16>,
    unmonitored: Option<bool>,
    tags: Option<String>,
    specials: Option<bool>,
    release_types: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct CalendarDiagnostics {
    // Non-null stored dates under these filters (any window) that are not valid instants.
    // They are never shown; the feed refuses to publish while this is non-zero.
    pub invalid_dates: i64,
    // TV only: episodes whose source air date is inside the window's dates but whose UTC air
    // time is unknown. They are not shown (no time is invented). `null` for movies.
    pub date_only_air_dates: Option<i64>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct CalendarPage<T> {
    pub media_type: MediaDomain,
    // Instant window `[start, end)`, RFC 3339 UTC; TV air times are compared to it.
    pub start: String,
    pub end: String,
    // Date window `[first_date, end_date)`; all-day movie releases are compared to it.
    pub first_date: String,
    pub end_date: String,
    pub include_unmonitored: bool,
    pub include_specials: bool,
    pub release_types: Vec<MovieReleaseType>,
    pub tags: Vec<i64>,
    pub items: Vec<T>,
    pub total: u32,
    pub limit: u32,
    pub evaluated_at: i64,
    pub diagnostics: CalendarDiagnostics,
}
#[derive(Serialize, ts_rs::TS)]
pub struct TvCalendarEntry {
    // Stable event identity (`episode-<id>`), independent of the date.
    pub event_id: String,
    pub target: MediaTarget,
    pub series_id: i64,
    pub series_title: String,
    pub series_type: Option<crate::library::SeriesType>,
    pub series_monitored: bool,
    pub network: Option<String>,
    pub season: i64,
    pub season_monitored: Option<bool>,
    pub number: i64,
    pub absolute_episode_number: Option<i64>,
    pub title: String,
    pub overview: Option<String>,
    pub finale_type: Option<String>,
    pub monitored: bool,
    // Episode and series monitored (the default calendar filter).
    pub effective_monitored: bool,
    // Source-local air date as stored (network calendar day); not converted.
    pub air_date: Option<String>,
    // Known UTC air time, RFC 3339.
    pub starts_at: String,
    pub runtime_minutes: Option<i64>,
    pub runtime_source: Option<CalendarRuntimeSource>,
    pub ends_at: Option<String>,
    pub has_file: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct MovieCalendarEntry {
    // Stable event identity (`movie-<id>-<release type>`), independent of the date.
    pub event_id: String,
    pub target: MediaTarget,
    pub metadata_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub release_type: MovieReleaseType,
    // All-day date (UTC date of the stored release timestamp); never shifted by viewer zone.
    pub date: String,
    // The stored release timestamp, RFC 3339 UTC.
    pub released_at: String,
    pub monitored: bool,
    pub has_file: bool,
    pub status: Option<String>,
    pub studio: Option<String>,
    pub runtime_minutes: Option<i64>,
}

pub fn router(db: Arc<Database>) -> Router {
    use MediaDomain::{Movies, Tv};
    Router::new()
        .route("/api/v1/tv/calendar", get(|s, q| list(s, q, Tv)))
        .route("/api/v1/movies/calendar", get(|s, q| list(s, q, Movies)))
        .route("/feed/v1/calendar/tv.ics", get(|s, q| feed(s, q, Tv)))
        .route(
            "/feed/v1/calendar/movies.ics",
            get(|s, q| feed(s, q, Movies)),
        )
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(REQUEST_DEADLINE, next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "calendar_unavailable",
            "Calendar request exceeded its deadline",
        )
        .into_response(),
    }
}

/// Filters shared by the JSON list and the feed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selector {
    media: MediaDomain,
    include_unmonitored: bool,
    include_specials: bool,
    tags: BTreeSet<i64>,
    release_types: BTreeSet<MovieReleaseType>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    first_date: NaiveDate,
    end_date: NaiveDate,
}
#[derive(Clone, Copy, Debug)]
struct Bound {
    instant: DateTime<Utc>,
    /// Civil date in the offset the client wrote (UTC for a plain date).
    date: NaiveDate,
    midnight: bool,
}
fn year_ok(year: i32) -> bool {
    (1..=9999).contains(&year)
}
/// A plain `YYYY-MM-DD` (UTC midnight, floating date) or a whole-second RFC 3339 instant.
fn bound(raw: &str) -> Option<Bound> {
    if raw.len() > MAX_TIMESTAMP_BYTES {
        return None;
    }
    if raw.len() == 10 {
        let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
        if date.format("%Y-%m-%d").to_string() != raw || !year_ok(date.year()) {
            return None;
        }
        return Some(Bound {
            instant: date.and_time(NaiveTime::MIN).and_utc(),
            date,
            midnight: true,
        });
    }
    let parsed = DateTime::parse_from_rfc3339(raw).ok()?;
    if parsed.timestamp_subsec_nanos() != 0 {
        return None;
    }
    let local = parsed.naive_local();
    let instant = parsed.to_utc();
    (year_ok(local.year()) && year_ok(instant.year())).then_some(Bound {
        instant,
        date: local.date(),
        midnight: local.time() == NaiveTime::MIN,
    })
}
/// Half-open windows. Movie dates overlap `[start, end)` in the client's written offset: the
/// first date is the start's civil date, the end date is the end's civil date when the end is
/// midnight and the following date otherwise.
fn window(start: Bound, end: Bound) -> Result<Window> {
    if start.instant >= end.instant {
        return Err(invalid("start must be before end"));
    }
    let end_date = if end.midnight {
        Some(end.date)
    } else {
        end.date.succ_opt()
    }
    .ok_or_else(|| invalid("end is outside the supported date range"))?;
    let days = (end_date - start.date).num_days();
    if days < 1 {
        return Err(invalid("window must cover at least one calendar date"));
    }
    if days > MAX_WINDOW_DAYS {
        return Err(invalid("window must not exceed 366 calendar dates"));
    }
    Ok(Window {
        start: start.instant,
        end: end.instant,
        first_date: start.date,
        end_date,
    })
}
fn digits(value: &str, max_len: usize) -> Option<i64> {
    (!value.is_empty() && value.len() <= max_len && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}
fn flag(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid("boolean options must be true or false")),
    }
}
fn tags(value: &str) -> Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    for part in value.split(',') {
        let id = digits(part, 16)
            .filter(|id| (1..=MAX_SAFE_ID).contains(id))
            .ok_or_else(|| invalid("tags must be comma-separated positive tag IDs"))?;
        if !out.insert(id) {
            return Err(invalid("tags must not repeat"));
        }
        if out.len() > MAX_TAGS {
            return Err(invalid("at most 64 tags may be supplied"));
        }
    }
    Ok(out)
}
fn release_types(value: &str) -> Result<BTreeSet<MovieReleaseType>> {
    let mut out = BTreeSet::new();
    for part in value.split(',') {
        let kind = MovieReleaseType::parse(part).ok_or_else(|| {
            invalid("release_types must be comma-separated in_cinemas, digital or physical")
        })?;
        if !out.insert(kind) {
            return Err(invalid("release_types must not repeat"));
        }
    }
    Ok(out)
}
/// Pairs from a raw query with the shared strictness rules; `apply` handles domain keys.
fn pairs(raw: Option<&str>) -> Result<Vec<(String, String)>> {
    let raw = raw.unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return Err(invalid("Calendar query exceeds 4096 bytes"));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    if raw.is_empty() {
        return Ok(out);
    }
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if key.is_empty() || value.is_empty() {
            return Err(invalid("Calendar query keys and values must be non-empty"));
        }
        if !seen.insert(key.to_string()) {
            return Err(invalid("Calendar query keys must not repeat"));
        }
        out.push((key.into_owned(), value.into_owned()));
    }
    Ok(out)
}
fn selector(media: MediaDomain) -> Selector {
    Selector {
        media,
        include_unmonitored: false,
        include_specials: true,
        tags: BTreeSet::new(),
        release_types: MovieReleaseType::ALL.into_iter().collect(),
    }
}
/// Returns false when the key is not a shared selector key.
fn apply(selector: &mut Selector, key: &str, value: &str) -> Result<bool> {
    match (selector.media, key) {
        (_, "unmonitored") => selector.include_unmonitored = flag(value)?,
        (_, "tags") => selector.tags = tags(value)?,
        (MediaDomain::Tv, "specials") => selector.include_specials = flag(value)?,
        (MediaDomain::Movies, "release_types") => selector.release_types = release_types(value)?,
        _ => return Ok(false),
    }
    Ok(true)
}
fn parse_list_query(raw: Option<&str>, media: MediaDomain) -> Result<(Selector, Window)> {
    let mut out = selector(media);
    let (mut start, mut end) = (None, None);
    for (key, value) in pairs(raw)? {
        match key.as_str() {
            "start" => {
                start = Some(bound(&value).ok_or_else(|| {
                    invalid("start must be YYYY-MM-DD or a whole-second RFC 3339 instant")
                })?)
            }
            "end" => {
                end = Some(bound(&value).ok_or_else(|| {
                    invalid("end must be YYYY-MM-DD or a whole-second RFC 3339 instant")
                })?)
            }
            _ => {
                if !apply(&mut out, &key, &value)? {
                    return Err(invalid("Unknown or inapplicable Calendar query key"));
                }
            }
        }
    }
    let (Some(start), Some(end)) = (start, end) else {
        return Err(invalid("start and end are required"));
    };
    Ok((out, window(start, end)?))
}
fn parse_feed_query(raw: Option<&str>, media: MediaDomain, now: i64) -> Result<(Selector, Window)> {
    let mut out = selector(media);
    let (mut past, mut future) = (DEFAULT_PAST_DAYS, DEFAULT_FUTURE_DAYS);
    for (key, value) in pairs(raw)? {
        match key.as_str() {
            "past_days" | "future_days" => {
                let days = digits(&value, 3)
                    .filter(|d| *d < MAX_WINDOW_DAYS)
                    .ok_or_else(|| invalid("past_days and future_days must be 0 to 365"))?;
                if key == "past_days" {
                    past = days
                } else {
                    future = days
                }
            }
            _ => {
                if !apply(&mut out, &key, &value)? {
                    return Err(invalid("Unknown or inapplicable Calendar feed query key"));
                }
            }
        }
    }
    if past + future + 1 > MAX_WINDOW_DAYS {
        return Err(invalid(
            "past_days + future_days + 1 must not exceed 366 dates",
        ));
    }
    let today = DateTime::from_timestamp(now, 0)
        .ok_or_else(clock_error)?
        .date_naive();
    let first = today
        .checked_sub_days(chrono::Days::new(past as u64))
        .ok_or_else(clock_error)?;
    let end = today
        .checked_add_days(chrono::Days::new(future as u64 + 1))
        .ok_or_else(clock_error)?;
    let bound = |date: NaiveDate| Bound {
        instant: date.and_time(NaiveTime::MIN).and_utc(),
        date,
        midnight: true,
    };
    Ok((out, window(bound(first), bound(end))?))
}
fn clock_error() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "calendar_clock_error",
        "System clock is outside the supported range",
    )
}
fn clock() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .filter(|v| *v < MAX_SAFE_ID)
        .ok_or_else(clock_error)
}

/// Strict reading of a stored UTC timestamp: RFC 3339 with offset, or a naive SQL timestamp
/// that the schema designates UTC. Years 1–9999, at most nine fractional digits.
pub(crate) fn stored_instant(raw: &str) -> Option<DateTime<Utc>> {
    if raw.len() > MAX_TIMESTAMP_BYTES
        || raw.split_once('.').is_some_and(|(_, fraction)| {
            fraction.bytes().take_while(u8::is_ascii_digit).count() > MAX_FRACTION_DIGITS
        })
    {
        return None;
    }
    let value = DateTime::parse_from_rfc3339(raw)
        .map(|v| v.to_utc())
        .ok()
        .or_else(|| {
            ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
                .into_iter()
                .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
                .map(|v| v.and_utc())
        })?;
    (year_ok(value.year()) && value.timestamp_subsec_nanos() < 1_000_000_000).then_some(value)
}
fn rfc3339(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}
fn day(value: &NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}
fn midnight_epoch(value: &NaiveDate) -> i64 {
    value.and_time(NaiveTime::MIN).and_utc().timestamp()
}
/// SQL validity for a stored timestamp column: readable by SQLite and with a real calendar
/// date portion (SQLite normalizes e.g. day 30 of February instead of rejecting it). Rust then
/// re-validates every selected value strictly.
fn valid_sql(column: &str) -> String {
    format!(
        "(unixepoch({column}) IS NOT NULL AND date(substr({column},1,10))=substr({column},1,10))"
    )
}
/// SQLite's second-valued `unixepoch()` can round a fractional timestamp across a second
/// boundary. Strip only the fraction (and preserve any UTC offset) so the SQL candidate is
/// the exact second containing the instant; whole-second window bounds then match Rust.
fn floor_unixepoch_sql(column: &str) -> String {
    format!(
        "unixepoch(CASE
            WHEN substr({column},-1)='Z' THEN substr({column},1,19)||'Z'
            WHEN substr({column},-6) GLOB '[+-][0-9][0-9]:[0-9][0-9]'
                THEN substr({column},1,19)||substr({column},-6)
            WHEN instr({column},'.')>0 THEN substr({column},1,19)
            ELSE {column}
        END)"
    )
}
fn id_list(ids: &BTreeSet<i64>) -> String {
    // Only validated integers are formatted into SQL.
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

const TV_COLUMNS: &str = "SELECT e.id,e.series_id,s.title,s.monitored,l.series_type,s.network,s.runtime,e.season,se.monitored,e.number,e.absolute_episode_number,e.title,e.overview,e.finale_type,e.monitored,e.air_date,e.air_date_utc,e.runtime,f.id";
const TV_FROM: &str = " FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN seasons se ON se.series_id=e.series_id AND se.number=e.season LEFT JOIN library_settings l ON l.series_id=s.id AND l.media_type='tv' LEFT JOIN episode_files f ON f.id=e.episode_file_id AND f.series_id=e.series_id AND {active}";
const MOVIE_FROM: &str = " FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id LEFT JOIN movie_files f ON f.movie_id=m.id AND {active}";

fn tv_from() -> String {
    TV_FROM.replace("{active}", crate::media_files::active_file_sql("tv"))
}
fn movie_from() -> String {
    MOVIE_FROM.replace("{active}", crate::media_files::active_file_sql("movies"))
}
/// Monitoring, specials and OR-of-tags filters. TV default requires episode and series
/// monitoring (season flags are reported, not filtered); movies use the movie flag.
fn filter_sql(selector: &Selector) -> String {
    let mut parts = Vec::new();
    match (selector.media, selector.include_unmonitored) {
        (_, true) => {}
        (MediaDomain::Tv, false) => parts.push("e.monitored=1 AND s.monitored=1".to_string()),
        (MediaDomain::Movies, false) => parts.push("m.monitored=1".to_string()),
    }
    if selector.media == MediaDomain::Tv && !selector.include_specials {
        parts.push("e.season<>0".into());
    }
    if !selector.tags.is_empty() {
        parts.push(match selector.media {
            MediaDomain::Tv => format!(
                "EXISTS(SELECT 1 FROM series_tags t WHERE t.series_id=s.id AND t.tag_id IN ({}))",
                id_list(&selector.tags)
            ),
            MediaDomain::Movies => format!(
                "EXISTS(SELECT 1 FROM movie_tags t WHERE t.movie_id=m.id AND t.tag_id IN ({}))",
                id_list(&selector.tags)
            ),
        });
    }
    if parts.is_empty() {
        "1".into()
    } else {
        parts.join(" AND ")
    }
}
async fn count(c: &Connection, sql: &str, params: impl libsql::params::IntoParams) -> Result<i64> {
    Ok(c.query(sql, params)
        .await?
        .next()
        .await?
        .ok_or_else(unavailable)?
        .get::<Option<i64>>(0)?
        .unwrap_or(0))
}
fn series_type(value: Option<String>) -> Result<Option<crate::library::SeriesType>> {
    use crate::library::SeriesType;
    Ok(match value.as_deref() {
        None => None,
        Some("standard") => Some(SeriesType::Standard),
        Some("daily") => Some(SeriesType::Daily),
        Some("anime") => Some(SeriesType::Anime),
        Some(_) => return Err(state_invalid()),
    })
}
fn positive(value: Option<i64>) -> Option<i64> {
    value.filter(|v| *v > 0)
}

struct Selection<T> {
    items: Vec<T>,
    diagnostics: CalendarDiagnostics,
}
async fn select_tv(
    c: &Connection,
    selector: &Selector,
    window: &Window,
    cap: usize,
) -> Result<Selection<TvCalendarEntry>> {
    let from = tv_from();
    let filter = filter_sql(selector);
    let valid = valid_sql("e.air_date_utc");
    let invalid_stored = count(
        c,
        &format!(
            "SELECT count(*){from} WHERE e.air_date_utc IS NOT NULL AND NOT {valid} AND {filter}"
        ),
        (),
    )
    .await?;
    let date_only = count(
        c,
        &format!("SELECT count(*){from} WHERE e.air_date_utc IS NULL AND e.air_date IS NOT NULL AND date(e.air_date)=e.air_date AND e.air_date>=?1 AND e.air_date<?2 AND {filter}"),
        libsql::params![day(&window.first_date), day(&window.end_date)],
    )
    .await?;
    let candidate = floor_unixepoch_sql("e.air_date_utc");
    let mut rows = c
        .query(
            &format!("{TV_COLUMNS}{from} WHERE e.air_date_utc IS NOT NULL AND {valid} AND {candidate}>=?1 AND {candidate}<?2 AND {filter} ORDER BY {candidate},e.id LIMIT ?3"),
            libsql::params![window.start.timestamp(), window.end.timestamp(), cap as i64 + 1],
        )
        .await?;
    let (mut items, mut fetched, mut invalid_selected) = (Vec::new(), 0usize, 0i64);
    while let Some(row) = rows.next().await? {
        fetched += 1;
        if fetched > cap {
            return Err(resource_limit());
        }
        let raw: String = row.get(16)?;
        // SQLite's reader is permissive (e.g. day 30 of February); Rust decides validity.
        let Some(start) = stored_instant(&raw) else {
            invalid_selected += 1;
            continue;
        };
        if start < window.start || start >= window.end {
            continue;
        }
        let id: i64 = row.get(0)?;
        let series_monitored = row.get::<i64>(3)? == 1;
        let monitored = row.get::<i64>(14)? == 1;
        let (runtime_minutes, runtime_source) = match (
            positive(row.get::<Option<i64>>(6)?),
            positive(row.get::<Option<i64>>(17)?),
        ) {
            (Some(series), _) => (Some(series), Some(CalendarRuntimeSource::Series)),
            (None, Some(episode)) => (Some(episode), Some(CalendarRuntimeSource::Episode)),
            (None, None) => (None, None),
        };
        let ends_at = runtime_minutes
            .and_then(|m| m.checked_mul(SECONDS_PER_MINUTE))
            .and_then(|s| start.checked_add_signed(chrono::TimeDelta::seconds(s)))
            .filter(|t| year_ok(t.year()))
            .map(|t| rfc3339(&t));
        items.push((
            start,
            TvCalendarEntry {
                event_id: format!("episode-{id}"),
                target: MediaTarget::Episode(id),
                series_id: row.get(1)?,
                series_title: row.get(2)?,
                series_type: series_type(row.get(4)?)?,
                series_monitored,
                network: row.get(5)?,
                season: row.get(7)?,
                season_monitored: row.get::<Option<i64>>(8)?.map(|v| v == 1),
                number: row.get(9)?,
                absolute_episode_number: row.get(10)?,
                title: row.get(11)?,
                overview: row.get(12)?,
                finale_type: row.get(13)?,
                monitored,
                effective_monitored: monitored && series_monitored,
                air_date: row.get(15)?,
                starts_at: rfc3339(&start),
                runtime_minutes,
                runtime_source,
                ends_at,
                has_file: row.get::<Option<i64>>(18)?.is_some(),
            },
        ));
    }
    items.sort_by(|a, b| {
        (
            a.0,
            a.1.series_id,
            a.1.season,
            a.1.number,
            id_of(&a.1.target),
        )
            .cmp(&(
                b.0,
                b.1.series_id,
                b.1.season,
                b.1.number,
                id_of(&b.1.target),
            ))
    });
    Ok(Selection {
        items: items.into_iter().map(|(_, entry)| entry).collect(),
        diagnostics: CalendarDiagnostics {
            invalid_dates: invalid_stored + invalid_selected,
            date_only_air_dates: Some(date_only),
        },
    })
}
fn id_of(target: &MediaTarget) -> i64 {
    match target {
        MediaTarget::Episode(id) | MediaTarget::Movie(id) => *id,
    }
}
async fn select_movies(
    c: &Connection,
    selector: &Selector,
    window: &Window,
    cap: usize,
) -> Result<Selection<MovieCalendarEntry>> {
    let from = movie_from();
    let filter = filter_sql(selector);
    if selector.release_types.is_empty() {
        return Err(invalid("release_types must not be empty"));
    }
    let invalid_expr = selector
        .release_types
        .iter()
        .map(|kind| {
            let col = kind.column();
            format!(
                "(d.{col} IS NOT NULL AND NOT {})",
                valid_sql(&format!("d.{col}"))
            )
        })
        .collect::<Vec<_>>()
        .join("+");
    let invalid_stored = count(
        c,
        &format!("SELECT sum({invalid_expr}){from} WHERE {filter}"),
        (),
    )
    .await?;
    let branches = selector
        .release_types
        .iter()
        .map(|kind| {
            let col = kind.column();
            let candidate = floor_unixepoch_sql(&format!("d.{col}"));
            format!("SELECT m.id,d.id,d.title,d.year,d.tmdb_id,d.imdb_id,m.monitored,d.status,d.studio,d.runtime,f.id,{},d.{col},{candidate}{from} WHERE d.{col} IS NOT NULL AND {} AND {candidate}>=?1 AND {candidate}<?2 AND {filter}", kind.order(), valid_sql(&format!("d.{col}")))
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let mut rows = c
        .query(
            &format!("{branches} ORDER BY 14,12,1 LIMIT ?3"),
            libsql::params![
                midnight_epoch(&window.first_date),
                midnight_epoch(&window.end_date),
                cap as i64 + 1
            ],
        )
        .await?;
    let (mut items, mut fetched, mut invalid_selected) = (Vec::new(), 0usize, 0i64);
    while let Some(row) = rows.next().await? {
        fetched += 1;
        if fetched > cap {
            return Err(resource_limit());
        }
        let kind = match row.get::<i64>(11)? {
            0 => MovieReleaseType::InCinemas,
            1 => MovieReleaseType::Digital,
            2 => MovieReleaseType::Physical,
            _ => return Err(state_invalid()),
        };
        let raw: String = row.get(12)?;
        let Some(released) = stored_instant(&raw) else {
            invalid_selected += 1;
            continue;
        };
        let date = released.date_naive();
        if date < window.first_date || date >= window.end_date {
            continue;
        }
        let id: i64 = row.get(0)?;
        items.push(MovieCalendarEntry {
            event_id: format!("movie-{id}-{}", kind.key()),
            target: MediaTarget::Movie(id),
            metadata_id: row.get(1)?,
            title: row.get(2)?,
            year: row.get(3)?,
            tmdb_id: row.get(4)?,
            imdb_id: row.get(5)?,
            release_type: kind,
            date: day(&date),
            released_at: rfc3339(&released),
            monitored: row.get::<i64>(6)? == 1,
            has_file: row.get::<Option<i64>>(10)?.is_some(),
            status: row.get(7)?,
            studio: row.get(8)?,
            runtime_minutes: row.get(9)?,
        });
    }
    items.sort_by(|a, b| {
        (&a.date, a.release_type, id_of(&a.target)).cmp(&(
            &b.date,
            b.release_type,
            id_of(&b.target),
        ))
    });
    Ok(Selection {
        items,
        diagnostics: CalendarDiagnostics {
            invalid_dates: invalid_stored + invalid_selected,
            date_only_air_dates: None,
        },
    })
}

fn page<T>(
    selector: &Selector,
    window: &Window,
    now: i64,
    selection: Selection<T>,
) -> CalendarPage<T> {
    CalendarPage {
        media_type: selector.media,
        start: rfc3339(&window.start),
        end: rfc3339(&window.end),
        first_date: day(&window.first_date),
        end_date: day(&window.end_date),
        include_unmonitored: selector.include_unmonitored,
        include_specials: selector.media == MediaDomain::Tv && selector.include_specials,
        release_types: match selector.media {
            MediaDomain::Tv => Vec::new(),
            MediaDomain::Movies => selector.release_types.iter().copied().collect(),
        },
        tags: selector.tags.iter().copied().collect(),
        total: selection.items.len() as u32,
        items: selection.items,
        limit: MAX_ENTRIES as u32,
        evaluated_at: now,
        diagnostics: selection.diagnostics,
    }
}
/// Runs `read` inside one read-only transaction and always releases the snapshot.
async fn read_only<T>(
    db: &Database,
    read: impl AsyncFnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome = read(&tx).await;
    tx.rollback().await?;
    outcome
}
async fn list(
    State(db): State<Arc<Database>>,
    RawQuery(raw): RawQuery,
    media: MediaDomain,
) -> Result<Response> {
    let (selector, window) = parse_list_query(raw.as_deref(), media)?;
    let now = clock()?;
    let body = read_only(&db, async |c: &Connection| {
        match media {
            MediaDomain::Tv => serde_json::to_vec(&page(
                &selector,
                &window,
                now,
                select_tv(c, &selector, &window, MAX_ENTRIES).await?,
            )),
            MediaDomain::Movies => serde_json::to_vec(&page(
                &selector,
                &window,
                now,
                select_movies(c, &selector, &window, MAX_ENTRIES).await?,
            )),
        }
        .map_err(|_| state_invalid())
    })
    .await?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(resource_limit());
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

fn episode_summary(entry: &TvCalendarEntry) -> String {
    match entry.series_type {
        // Daily series are identified by date; the reference feed omits numbering.
        Some(crate::library::SeriesType::Daily) => {
            format!("{} - {}", entry.series_title, entry.title)
        }
        _ => format!(
            "{} - {}x{:02} - {}",
            entry.series_title, entry.season, entry.number, entry.title
        ),
    }
}
fn tv_events(items: &[TvCalendarEntry]) -> Result<Vec<ical::Event>> {
    items
        .iter()
        .map(|entry| {
            Ok(ical::Event {
                uid: format!("{}@hrrdarr", entry.event_id),
                when: ical::When::Timed {
                    start: stored_instant(&entry.starts_at).ok_or_else(state_invalid)?,
                    minutes: entry.runtime_minutes,
                },
                summary: episode_summary(entry),
                description: entry.overview.clone(),
                category: entry.network.clone(),
                status: if entry.has_file {
                    ical::Status::Confirmed
                } else {
                    ical::Status::Tentative
                },
            })
        })
        .collect()
}
fn movie_events(items: &[MovieCalendarEntry]) -> Result<Vec<ical::Event>> {
    items
        .iter()
        .map(|entry| {
            Ok(ical::Event {
                uid: format!("{}@hrrdarr", entry.event_id),
                when: ical::When::AllDay(
                    NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
                        .map_err(|_| state_invalid())?,
                ),
                summary: format!("{} - {}", entry.title, entry.release_type.label()),
                description: None,
                category: entry.studio.clone(),
                // Unannounced/announced catalog status is not a confirmed release.
                status: if matches!(entry.status.as_deref(), Some("tba" | "announced")) {
                    ical::Status::Tentative
                } else {
                    ical::Status::Confirmed
                },
            })
        })
        .collect()
}
async fn feed(
    State(db): State<Arc<Database>>,
    RawQuery(raw): RawQuery,
    media: MediaDomain,
) -> Result<Response> {
    let now = clock()?;
    let (selector, window) = parse_feed_query(raw.as_deref(), media, now)?;
    let (events, invalid_dates) = read_only(&db, async |c: &Connection| {
        Ok(match media {
            MediaDomain::Tv => {
                let s = select_tv(c, &selector, &window, MAX_FEED_ENTRIES).await?;
                (tv_events(&s.items)?, s.diagnostics.invalid_dates)
            }
            MediaDomain::Movies => {
                let s = select_movies(c, &selector, &window, MAX_FEED_ENTRIES).await?;
                (movie_events(&s.items)?, s.diagnostics.invalid_dates)
            }
        })
    })
    .await?;
    if invalid_dates > 0 {
        return Err(feed_incomplete());
    }
    let stamp = DateTime::from_timestamp(now, 0).ok_or_else(clock_error)?;
    let name = match media {
        MediaDomain::Tv => "hrrdarr TV",
        MediaDomain::Movies => "hrrdarr Movies",
    };
    let body = ical::render(name, &stamp, &events).ok_or_else(state_invalid)?;
    if body.len() > MAX_FEED_BYTES {
        return Err(resource_limit());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/calendar; charset=utf-8"),
            // Library membership is private data; never cache it in shared caches.
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_q(raw: &str, media: MediaDomain) -> Result<(Selector, Window)> {
        parse_list_query(Some(raw), media)
    }
    fn code(result: Result<(Selector, Window)>) -> &'static str {
        result.err().map(|e| e.1).unwrap_or("ok")
    }

    #[test]
    fn plain_dates_are_floating_utc_midnights() {
        let (s, w) = list_q("start=2026-10-01&end=2026-11-01", MediaDomain::Movies).unwrap();
        assert_eq!(rfc3339(&w.start), "2026-10-01T00:00:00Z");
        assert_eq!(rfc3339(&w.end), "2026-11-01T00:00:00Z");
        assert_eq!(
            (day(&w.first_date), day(&w.end_date)),
            ("2026-10-01".into(), "2026-11-01".into())
        );
        assert!(!s.include_unmonitored);
        assert_eq!(s.release_types.len(), 3);
    }

    #[test]
    fn offset_bounds_keep_the_written_civil_dates_for_all_day_entries() {
        // Local midnight in UTC-04:00 across a DST change to UTC-05:00.
        let (_, w) = list_q(
            "start=2026-10-01T00:00:00-04:00&end=2026-11-02T00:00:00-05:00",
            MediaDomain::Tv,
        )
        .unwrap();
        assert_eq!(rfc3339(&w.start), "2026-10-01T04:00:00Z");
        assert_eq!(rfc3339(&w.end), "2026-11-02T05:00:00Z");
        assert_eq!(
            (day(&w.first_date), day(&w.end_date)),
            ("2026-10-01".into(), "2026-11-02".into())
        );
        // A non-midnight end covers its whole date for all-day overlap.
        let (_, w) = list_q(
            "start=2026-10-01T12:00:00Z&end=2026-10-01T13:00:00Z",
            MediaDomain::Movies,
        )
        .unwrap();
        assert_eq!(
            (day(&w.first_date), day(&w.end_date)),
            ("2026-10-01".into(), "2026-10-02".into())
        );
    }

    #[test]
    fn rejects_malformed_reversed_overlong_and_inapplicable_queries() {
        for (raw, media) in [
            ("", MediaDomain::Tv),
            ("start=2026-10-01", MediaDomain::Tv),
            ("start=2026-10-02&end=2026-10-01", MediaDomain::Tv),
            ("start=2026-10-01&end=2026-10-01", MediaDomain::Tv),
            ("start=2026-02-30&end=2026-03-05", MediaDomain::Tv),
            ("start=2026-1-01&end=2026-03-05", MediaDomain::Tv),
            (
                "start=2026-10-01T00:00:00.5Z&end=2026-10-05",
                MediaDomain::Tv,
            ),
            ("start=2026-10-01T00:00:00&end=2026-10-05", MediaDomain::Tv),
            // An unencoded '+' decodes to a space and is rejected rather than misread.
            (
                "start=2026-10-01T00:00:00+02:00&end=2026-10-05",
                MediaDomain::Tv,
            ),
            ("start=2026-01-01&end=2027-01-03", MediaDomain::Tv),
            (
                "start=2026-10-01&end=2026-10-05&start=2026-10-02",
                MediaDomain::Tv,
            ),
            ("start=2026-10-01&end=2026-10-05&page=1", MediaDomain::Tv),
            (
                "start=2026-10-01&end=2026-10-05&unmonitored=1",
                MediaDomain::Tv,
            ),
            (
                "start=2026-10-01&end=2026-10-05&unmonitored=",
                MediaDomain::Tv,
            ),
            (
                "start=2026-10-01&end=2026-10-05&release_types=digital",
                MediaDomain::Tv,
            ),
            (
                "start=2026-10-01&end=2026-10-05&specials=false",
                MediaDomain::Movies,
            ),
            (
                "start=2026-10-01&end=2026-10-05&release_types=digital,digital",
                MediaDomain::Movies,
            ),
            (
                "start=2026-10-01&end=2026-10-05&release_types=theatrical",
                MediaDomain::Movies,
            ),
            (
                "start=2026-10-01&end=2026-10-05&tags=1,,2",
                MediaDomain::Movies,
            ),
            (
                "start=2026-10-01&end=2026-10-05&tags=0",
                MediaDomain::Movies,
            ),
            (
                "start=2026-10-01&end=2026-10-05&tags=9007199254740992",
                MediaDomain::Movies,
            ),
            ("start=0000-12-31&end=0001-01-02", MediaDomain::Movies),
        ] {
            assert_eq!(code(list_q(raw, media)), "invalid_calendar_query", "{raw}");
        }
        let tags = (1..=65)
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            code(list_q(
                &format!("start=2026-10-01&end=2026-10-05&tags={tags}"),
                MediaDomain::Tv
            )),
            "invalid_calendar_query"
        );
        let long = format!(
            "start=2026-10-01&end=2026-10-05&x={}",
            "y".repeat(MAX_QUERY_BYTES)
        );
        assert_eq!(
            code(list_q(&long, MediaDomain::Tv)),
            "invalid_calendar_query"
        );
    }

    #[test]
    fn accepts_documented_options_and_the_366_date_maximum() {
        let (s, _) = list_q(
            "start=2024-01-01&end=2025-01-01&unmonitored=true&specials=false&tags=3,1",
            MediaDomain::Tv,
        )
        .unwrap();
        assert!(s.include_unmonitored && !s.include_specials);
        assert_eq!(s.tags.into_iter().collect::<Vec<_>>(), vec![1, 3]);
        let (s, _) = list_q(
            "start=2026-10-01T00:00:00%2B14:00&end=2026-10-02T00:00:00-12:00&release_types=physical,in_cinemas",
            MediaDomain::Movies,
        )
        .unwrap();
        assert_eq!(
            s.release_types.into_iter().collect::<Vec<_>>(),
            vec![MovieReleaseType::InCinemas, MovieReleaseType::Physical]
        );
    }

    #[test]
    fn feed_window_is_bounded_utc_dates_around_today() {
        let now = 1_792_324_800; // 2026-10-18T12:00:00Z
        let (_, w) = parse_feed_query(None, MediaDomain::Tv, now).unwrap();
        assert_eq!(
            (day(&w.first_date), day(&w.end_date)),
            ("2026-10-11".into(), "2026-11-16".into())
        );
        let (_, w) =
            parse_feed_query(Some("past_days=0&future_days=0"), MediaDomain::Movies, now).unwrap();
        assert_eq!(
            (day(&w.first_date), day(&w.end_date)),
            ("2026-10-18".into(), "2026-10-19".into())
        );
        for raw in [
            "past_days=365&future_days=1",
            "past_days=366",
            "future_days=-1",
            "start=2026-10-01",
            "past_days=1&past_days=2",
        ] {
            assert_eq!(
                parse_feed_query(Some(raw), MediaDomain::Tv, now)
                    .err()
                    .map(|e| e.1),
                Some("invalid_calendar_query"),
                "{raw}"
            );
        }
        assert!(
            parse_feed_query(Some("past_days=200&future_days=165"), MediaDomain::Tv, now).is_ok()
        );
    }

    #[test]
    fn stored_instants_accept_both_producer_encodings_strictly() {
        for (raw, expected) in [
            ("2026-10-01T01:02:03Z", "2026-10-01T01:02:03Z"),
            ("2026-10-01 01:02:03", "2026-10-01T01:02:03Z"),
            ("2026-10-01 01:02:03.25", "2026-10-01T01:02:03.250Z"),
            ("2026-10-01T23:30:00-02:00", "2026-10-02T01:30:00Z"),
            ("2024-02-29T00:00:00Z", "2024-02-29T00:00:00Z"),
        ] {
            assert_eq!(
                stored_instant(raw).map(|t| rfc3339(&t)).as_deref(),
                Some(expected),
                "{raw}"
            );
        }
        for raw in [
            "2026-02-30 00:00:00",
            "2026-10-01",
            "2026-10-01 25:00:00",
            "garbage",
            "2026-10-01 01:02:03.1234567891",
            "2025-02-29T00:00:00Z",
        ] {
            assert!(stored_instant(raw).is_none(), "{raw}");
        }
    }

    #[tokio::test]
    async fn fractional_boundary_timestamps_stay_in_the_exact_window_without_invalid_diagnostics() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let c = db.connect().unwrap();
        c.execute_batch(
            "CREATE TABLE series(id INTEGER PRIMARY KEY,title TEXT,monitored INTEGER,network TEXT,runtime INTEGER);
             CREATE TABLE seasons(series_id INTEGER,number INTEGER,monitored INTEGER);
             CREATE TABLE library_settings(media_type TEXT,series_id INTEGER,series_type TEXT);
             CREATE TABLE episode_files(id INTEGER PRIMARY KEY,series_id INTEGER);
             CREATE TABLE episodes(id INTEGER PRIMARY KEY,series_id INTEGER,season INTEGER,number INTEGER,title TEXT,monitored INTEGER,air_date TEXT,air_date_utc TEXT,runtime INTEGER,episode_file_id INTEGER,overview TEXT,finale_type TEXT,absolute_episode_number INTEGER);
             CREATE TABLE rss_candidate_imports(old_episode_file_id INTEGER,operation_id TEXT,retirement_state TEXT);
             CREATE TABLE same_path_replacements(operation_id TEXT,state TEXT);
             CREATE TABLE import_journal(operation_id TEXT,phase TEXT);
             CREATE TABLE movie_metadata(id INTEGER PRIMARY KEY,title TEXT,year INTEGER,tmdb_id INTEGER,imdb_id TEXT,status TEXT,studio TEXT,runtime INTEGER,in_cinemas TEXT,digital_release TEXT,physical_release TEXT);
             CREATE TABLE movies(id INTEGER PRIMARY KEY,metadata_id INTEGER,monitored INTEGER);
             CREATE TABLE movie_files(id INTEGER PRIMARY KEY,movie_id INTEGER);
             INSERT INTO series VALUES(1,'Series',1,NULL,NULL);
             INSERT INTO seasons VALUES(1,1,1);
             INSERT INTO library_settings VALUES('tv',1,'standard');
             INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date,air_date_utc) VALUES(1,1,1,1,'Episode',1,'2026-10-31','2026-10-31T23:59:59.9995Z');
             INSERT INTO movie_metadata(id,title,year,digital_release) VALUES(1,'Movie',2026,'2026-10-31T23:59:59.9995Z');
             INSERT INTO movies VALUES(1,1,1);",
        )
        .await
        .unwrap();

        let (tv, october) = list_q("start=2026-10-01&end=2026-11-01", MediaDomain::Tv).unwrap();
        let (_, november) = list_q("start=2026-11-01&end=2026-12-01", MediaDomain::Tv).unwrap();
        let october = select_tv(&c, &tv, &october, MAX_ENTRIES).await.unwrap();
        let november = select_tv(&c, &tv, &november, MAX_ENTRIES).await.unwrap();
        assert_eq!(october.items.len(), 1);
        assert!(november.items.is_empty());
        assert_eq!(october.diagnostics.invalid_dates, 0);
        assert_eq!(november.diagnostics.invalid_dates, 0);

        let (movies, october) =
            list_q("start=2026-10-01&end=2026-11-01", MediaDomain::Movies).unwrap();
        let (_, november) = list_q("start=2026-11-01&end=2026-12-01", MediaDomain::Movies).unwrap();
        let october = select_movies(&c, &movies, &october, MAX_ENTRIES)
            .await
            .unwrap();
        let november = select_movies(&c, &movies, &november, MAX_ENTRIES)
            .await
            .unwrap();
        assert_eq!(october.items.len(), 1);
        assert!(november.items.is_empty());
        assert_eq!(october.diagnostics.invalid_dates, 0);
        assert_eq!(november.diagnostics.invalid_dates, 0);
    }
}
