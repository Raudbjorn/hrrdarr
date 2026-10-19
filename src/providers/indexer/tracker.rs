//! Tracker indexers: Nyaa, Fanzub, IPTorrents, Torrentleech, BroadcastheNet, FileList, HDBits,
//! PassThePopcorn and TorrentPotato.
//!
//! None of these trackers publishes a Torznab capability document. Each one is implemented from its
//! public protocol (RSS 2.0 feeds, BroadcastheNet JSON-RPC, FileList / HDBits / PassThePopcorn /
//! TorrentPotato JSON), independently of the reference applications. A tracker answers one bounded
//! document per request (the transport caps bodies at 1 MiB); the document is paged locally with the
//! shared `offset`/`limit` contract. Every request goes through the shared transport, so redirects,
//! timeouts, body caps, per-provider lanes and the rate-limit cooldown apply unchanged.
//!
//! Failure policy (fixed, tested): an unusable document (not the expected JSON/XML shape, an
//! authentication error code, more rows than the cap, or rows that are *all* invalid) fails the whole
//! fetch with a typed error; otherwise a malformed row is dropped on its own and counted as an item
//! warning, exactly like the generic feed. Secrets (API keys, passkeys, cookies, account passwords)
//! live only in sealed credentials and in the request; they are redacted from release titles and
//! never appear in errors, test results or logs. Locators that embed a passkey (download links) stay
//! in the private release payload.
use super::{
    IndexerContinuation, IndexerError, IndexerItemWarning, IndexerItemWarningCode, IndexerSearch,
    RawIndexerPage, Release, ReleaseFacts, ReleaseIdentifiers, ReleaseMetadata, Result,
    TorrentFacts, TvNumbering, clean_title, cooldown,
    feed::{self, FeedDomainTest, FeedOptions, FeedTest},
    locator, redact_secrets, valid_text,
};
use crate::api::MediaDomain;
use crate::providers::{
    IndexerAccess, ProviderSettings, TrackerKind, TrackerScope,
    http::{HttpError, HttpOperation, HttpRequestBody},
};
use chrono::{Datelike, TimeZone};
use roxmltree::Node;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Rows requested from a JSON tracker per call.
const FETCH_CEILING: u32 = 100;
const MAX_ITEMS: u32 = 500;
const TITLE_BYTES: usize = 2048;
const QUERY_BYTES: usize = 256;
/// BroadcastheNet documents an hourly call budget; a call-limit error pauses the lane this long.
const BTN_CALL_LIMIT_SECONDS: u32 = 900;
/// Torznab root categories used when a tracker reports no category of its own.
const TV_ROOT: u32 = 5000;
const MOVIE_ROOT: u32 = 2000;
const TV_ANIME: u32 = 5070;

pub(super) struct Fetched {
    pub items: Vec<Release>,
    pub warnings: Vec<IndexerItemWarning>,
    pub below_minimum_seeders: u32,
}

struct Target<'a> {
    kind: TrackerKind,
    endpoint: &'a str,
    scope: &'a TrackerScope,
    domain: MediaDomain,
}

fn target(settings: &ProviderSettings, domain: MediaDomain) -> Result<Target<'_>> {
    let (kind, tv, movies) = settings.tracker_parts().ok_or(IndexerError::Unsupported)?;
    let scope = match domain {
        MediaDomain::Tv => tv.as_ref(),
        MediaDomain::Movies => movies.as_ref(),
    }
    .ok_or(IndexerError::Unsupported)?;
    Ok(Target {
        kind,
        endpoint: settings.endpoint(),
        scope,
        domain,
    })
}

fn request_domain(request: &IndexerSearch) -> MediaDomain {
    match request {
        IndexerSearch::Rss { media_type, .. } => *media_type,
        IndexerSearch::Tv { .. } => MediaDomain::Tv,
        IndexerSearch::Movie { .. } => MediaDomain::Movies,
    }
}
fn request_paging(request: &IndexerSearch) -> (u32, u32, u32) {
    match request {
        IndexerSearch::Rss {
            offset,
            query_index,
            limit,
            ..
        }
        | IndexerSearch::Tv {
            offset,
            query_index,
            limit,
            ..
        }
        | IndexerSearch::Movie {
            offset,
            query_index,
            limit,
            ..
        } => (*offset, *query_index, *limit),
    }
}

// ---------------------------------------------------------------------------------------------
// Transport helpers
// ---------------------------------------------------------------------------------------------

fn body_of(response: crate::providers::http::HttpResponse) -> Result<Vec<u8>> {
    if !(200..300).contains(&response.status) {
        return Err(IndexerError::Transport(HttpError::Transport));
    }
    Ok(response.body)
}
async fn get(
    operation: &HttpOperation<'_>,
    url: &str,
    params: &[(String, String)],
    headers: &[(String, String)],
) -> Result<Vec<u8>> {
    body_of(
        operation
            .request(url, params, headers, HttpRequestBody::Empty)
            .await?,
    )
}
async fn post_json(
    operation: &HttpOperation<'_>,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(body).map_err(|_| IndexerError::InvalidRequest)?;
    body_of(
        operation
            .request(url, &[], headers, HttpRequestBody::Json(bytes))
            .await?,
    )
}
fn text_body(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| IndexerError::InvalidResponse)
}
fn json_body(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| IndexerError::InvalidResponse)
}
fn origin(endpoint: &str) -> Result<String> {
    let url = url::Url::parse(endpoint).map_err(|_| IndexerError::InvalidRequest)?;
    let host = url.host_str().ok_or(IndexerError::InvalidRequest)?;
    Ok(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}
fn private_parameters(access: &IndexerAccess<'_>, domain: MediaDomain) -> Vec<(String, String)> {
    match domain {
        MediaDomain::Tv => access.tv_parameters,
        MediaDomain::Movies => access.movie_parameters,
    }
    .iter()
    .map(|p| (p.name.clone(), p.value.clone()))
    .collect()
}
fn cookie_header(access: &IndexerAccess<'_>) -> Vec<(String, String)> {
    access
        .cookie
        .map(|cookie| vec![("cookie".to_string(), cookie.to_string())])
        .unwrap_or_default()
}
fn credentials<'a>(access: &'a IndexerAccess<'_>) -> Result<(&'a str, &'a str)> {
    // A private tracker without an account is an authentication failure, found before any request.
    match (access.username, access.password) {
        (Some(user), Some(secret)) => Ok((user, secret)),
        _ => Err(IndexerError::Authentication),
    }
}

// ---------------------------------------------------------------------------------------------
// Query building
// ---------------------------------------------------------------------------------------------

fn query_text(value: &str) -> Result<String> {
    let cleaned = clean_title(value);
    if !valid_text(&cleaned, QUERY_BYTES) {
        return Err(IndexerError::InvalidRequest);
    }
    Ok(cleaned)
}
/// The episode designator appended to a title for trackers that only search by text.
fn episode_tag(numbering: &TvNumbering) -> Option<String> {
    Some(match numbering {
        TvNumbering::Episode { season, episode } => format!("S{season:02}E{episode:02}"),
        TvNumbering::Season { season } | TvNumbering::AnimeSeason { season, .. } => {
            format!("S{season:02}")
        }
        TvNumbering::Daily { date } => date.replace('-', " "),
        TvNumbering::DailySeason { year } => year.to_string(),
        TvNumbering::Special { episode_title } => clean_title(episode_title),
        TvNumbering::Anime {
            absolute_episode, ..
        } => format!("{absolute_episode:02}"),
    })
    .filter(|tag| !tag.is_empty())
}
fn tv_text(title: &str, numbering: &TvNumbering) -> Result<String> {
    let mut text = query_text(title)?;
    if let Some(tag) = episode_tag(numbering) {
        text.push(' ');
        text.push_str(&tag);
    }
    if !valid_text(&text, QUERY_BYTES) {
        return Err(IndexerError::InvalidRequest);
    }
    Ok(text)
}
fn movie_text(title: &str, year: Option<u16>) -> Result<String> {
    let mut text = query_text(title)?;
    if let Some(year) = year {
        text.push_str(&format!(" {year}"));
    }
    Ok(text)
}
fn imdb_digits(value: &str) -> Result<u64> {
    let digits = value.strip_prefix("tt").unwrap_or(value);
    if !(7..=10).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(IndexerError::InvalidRequest);
    }
    digits.parse().map_err(|_| IndexerError::InvalidRequest)
}
fn imdb_id(number: u64) -> String {
    format!("tt{number:07}")
}

// ---------------------------------------------------------------------------------------------
// JSON field helpers (trackers mix numbers and numeric strings freely)
// ---------------------------------------------------------------------------------------------

fn number(row: &Value, key: &str) -> Result<Option<u64>> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().map(Some).ok_or(IndexerError::InvalidResponse),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| IndexerError::InvalidResponse),
        Some(_) => Err(IndexerError::InvalidResponse),
    }
}
fn count(row: &Value, key: &str) -> Result<Option<u32>> {
    number(row, key)?
        .map(|n| u32::try_from(n).map_err(|_| IndexerError::InvalidResponse))
        .transpose()
}
fn string(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}
fn flag(row: &Value, key: &str) -> Option<bool> {
    match row.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => n.as_u64().map(|n| n != 0),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
fn unix_date(seconds: u64) -> Result<String> {
    let seconds = i64::try_from(seconds).map_err(|_| IndexerError::InvalidResponse)?;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .ok_or(IndexerError::InvalidResponse)
}
/// A `YYYY-MM-DD HH:MM:SS` value in UTC.
fn utc_text_date(value: &str) -> Result<String> {
    chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S")
        .map(|d| {
            d.and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .map_err(|_| IndexerError::InvalidResponse)
}
fn last_sunday(year: i32, month: u32) -> Option<chrono::NaiveDate> {
    let mut day = chrono::NaiveDate::from_ymd_opt(year, month, 31)?;
    while day.weekday() != chrono::Weekday::Sun {
        day = day.pred_opt()?;
    }
    Some(day)
}
/// FileList stamps uploads in Romanian local time (UTC+2, UTC+3 from the last Sunday of March 01:00 UTC
/// until the last Sunday of October 01:00 UTC). The fold hour at the autumn change reads as summer time.
fn bucharest_date(value: &str) -> Result<String> {
    let local = chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S")
        .map_err(|_| IndexerError::InvalidResponse)?;
    let year = local.date().year();
    let at = |date: Option<chrono::NaiveDate>| {
        date.and_then(|d| d.and_hms_opt(1, 0, 0))
            .map(|d| d.and_utc())
    };
    let (start, end) = at(last_sunday(year, 3))
        .zip(at(last_sunday(year, 10)))
        .ok_or(IndexerError::InvalidResponse)?;
    let summer = chrono::Utc.from_utc_datetime(&(local - chrono::Duration::hours(3)));
    let utc = if summer >= start && summer < end {
        summer
    } else {
        chrono::Utc.from_utc_datetime(&(local - chrono::Duration::hours(2)))
    };
    Ok(utc.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

// ---------------------------------------------------------------------------------------------
// Release assembly
// ---------------------------------------------------------------------------------------------

struct Row {
    kind: TrackerKind,
    id: String,
    title: String,
    size: Option<u64>,
    published_at: String,
    seeders: Option<u32>,
    leechers: Option<u32>,
    download_url: String,
    info_url: Option<String>,
    info_hash: Option<String>,
    imdb: Option<String>,
    tvdb: Option<u32>,
    free: Option<bool>,
    internal: Option<bool>,
    scene: Option<bool>,
    category: u32,
}
fn build(row: Row, domain: MediaDomain) -> Result<Release> {
    if !valid_text(&row.title, TITLE_BYTES) || row.id.is_empty() || row.id.len() > 128 {
        return Err(IndexerError::InvalidResponse);
    }
    let download_url = locator(&row.download_url, true)?;
    let info_url = row
        .info_url
        .as_deref()
        .and_then(|url| locator(url, false).ok());
    let info_hash = row
        .info_hash
        .map(|hash| {
            if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(hash.to_ascii_lowercase())
            } else {
                Err(IndexerError::InvalidResponse)
            }
        })
        .transpose()?;
    let peers = row
        .seeders
        .zip(row.leechers)
        .map(|(s, l)| s.checked_add(l).ok_or(IndexerError::InvalidResponse))
        .transpose()?;
    let identifiers = match domain {
        MediaDomain::Tv => ReleaseIdentifiers::Tv {
            tvdb_id: row.tvdb,
            tvmaze_id: None,
            tvrage_id: None,
            tmdb_id: None,
            imdb_id: row.imdb,
        },
        MediaDomain::Movies => ReleaseIdentifiers::Movie {
            tmdb_id: None,
            imdb_id: row.imdb,
        },
    };
    Ok(Release {
        metadata: ReleaseMetadata {
            title: Some(row.title),
            size_bytes: row.size.filter(|n| *n > 0),
            published_at: row.published_at,
            categories: vec![row.category],
            seeders: row.seeders,
            leechers: row.leechers,
            peers,
            languages: Vec::new(),
        },
        guid: Some(format!("{}:{}", row.kind.name(), row.id)),
        download_url,
        facts: ReleaseFacts {
            identifiers,
            scene: row.scene,
            nuked: None,
            subtitles: Vec::new(),
            has_subtitles: None,
            comments_url: None,
            info_url,
            torrent: Some(TorrentFacts {
                info_hash,
                magnet_url: None,
                download_volume_factor: row.free.map(|free| if free { 0.0 } else { 1.0 }),
                upload_volume_factor: None,
                minimum_ratio: None,
                minimum_seed_seconds: None,
                internal: row.internal,
            }),
        },
        attributes: BTreeMap::new(),
    })
}
fn rows_to_releases<T>(
    rows: impl Iterator<Item = T>,
    domain: MediaDomain,
    mut parse: impl FnMut(T) -> Result<Row>,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut raw = 0usize;
    for row in rows {
        if raw >= MAX_ITEMS as usize {
            return Err(IndexerError::InvalidResponse);
        }
        let index = raw as u32;
        raw += 1;
        match parse(row).and_then(|row| build(row, domain)) {
            Ok(release) if release.guid.as_ref().is_none_or(|g| ids.insert(g.clone())) => {
                items.push(release)
            }
            Ok(_) | Err(_) => warnings.push(IndexerItemWarning {
                index,
                code: IndexerItemWarningCode::InvalidItem,
            }),
        }
    }
    if items.is_empty() && !warnings.is_empty() {
        return Err(IndexerError::InvalidResponse);
    }
    Ok((items, warnings))
}

// ---------------------------------------------------------------------------------------------
// RSS trackers: Nyaa, IPTorrents, Torrentleech (torrent) and Fanzub (NZB)
// ---------------------------------------------------------------------------------------------

fn merged(
    defaults: &[(&str, &str)],
    private: Vec<(String, String)>,
    extra: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = defaults
        .iter()
        .filter(|(name, _)| {
            !private
                .iter()
                .any(|(own, _)| own.eq_ignore_ascii_case(name))
        })
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    params.extend(private);
    params.extend(extra);
    params
}

async fn fetch_rss(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let base = url::Url::parse(t.endpoint).map_err(|_| IndexerError::InvalidRequest)?;
    let search = |text: String| vec![("q".to_string(), text)];
    let (url, params, options) = match t.kind {
        TrackerKind::Nyaa => {
            let extra = match request {
                IndexerSearch::Rss { .. } => vec![],
                IndexerSearch::Tv {
                    title, numbering, ..
                } => search(tv_text(title, numbering)?),
                IndexerSearch::Movie { title, year, .. } => search(movie_text(title, *year)?),
            };
            (
                t.endpoint.to_string(),
                merged(
                    &[("page", "rss"), ("c", "1_0"), ("f", "1")],
                    private_parameters(access, t.domain),
                    extra,
                ),
                FeedOptions {
                    description_size: false,
                    default_category: (t.domain == MediaDomain::Tv).then_some(TV_ANIME),
                },
            )
        }
        TrackerKind::Fanzub => {
            let extra = match request {
                IndexerSearch::Rss { .. } => vec![],
                IndexerSearch::Tv {
                    title, numbering, ..
                } => search(tv_text(title, numbering)?),
                IndexerSearch::Movie { .. } => return Err(IndexerError::Unsupported),
            };
            let (url, params) = (
                t.endpoint.to_string(),
                merged(
                    &[("cat", "anime"), ("max", "100")],
                    private_parameters(access, t.domain),
                    extra,
                ),
            );
            let body = text_body(get(operation, &url, &params, &[]).await?)?;
            return cooldown(operation, parse_nzb_feed(&body, &base, t.domain));
        }
        TrackerKind::Iptorrents => {
            if !matches!(request, IndexerSearch::Rss { .. }) {
                return Err(IndexerError::Unsupported);
            }
            (
                t.endpoint.to_string(),
                private_parameters(access, t.domain),
                FeedOptions {
                    description_size: true,
                    default_category: None,
                },
            )
        }
        TrackerKind::Torrentleech => {
            if !matches!(request, IndexerSearch::Rss { .. }) {
                return Err(IndexerError::Unsupported);
            }
            let key = access.api_key.ok_or(IndexerError::Authentication)?;
            let mut keyed = base.clone();
            keyed
                .path_segments_mut()
                .map_err(|_| IndexerError::InvalidRequest)?
                .pop_if_empty()
                .push(key);
            (
                keyed.to_string(),
                private_parameters(access, t.domain),
                FeedOptions {
                    description_size: true,
                    default_category: None,
                },
            )
        }
        _ => return Err(IndexerError::Unsupported),
    };
    let body = text_body(get(operation, &url, &params, &cookie_header(access)).await?)?;
    let parsed = cooldown(
        operation,
        feed::parse_feed_with(&body, &base, t.domain, options),
    )?;
    Ok((parsed.items, parsed.warnings))
}

/// Fanzub: an RSS 2.0 feed whose items carry an NZB enclosure. There is no torrent, no seeder data
/// and no category vocabulary: everything it lists is anime.
fn parse_nzb_feed(
    input: &str,
    base: &url::Url,
    domain: MediaDomain,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let doc = super::xml(input.strip_prefix('\u{feff}').unwrap_or(input))?;
    let root = doc.root_element();
    if root.tag_name().name() != "rss" || root.tag_name().namespace().is_some() {
        return Err(IndexerError::InvalidResponse);
    }
    let channel = super::child(root, "channel")?.ok_or(IndexerError::InvalidResponse)?;
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut guids = std::collections::BTreeSet::new();
    let mut raw = 0usize;
    for item in channel.children().filter(|n| n.has_tag_name("item")) {
        if raw >= MAX_ITEMS as usize {
            return Err(IndexerError::InvalidResponse);
        }
        let index = raw as u32;
        raw += 1;
        match nzb_item(item, base, domain) {
            Ok(release)
                if release
                    .guid
                    .as_ref()
                    .is_none_or(|guid| guids.insert(guid.clone())) =>
            {
                items.push(release)
            }
            Ok(_) | Err(_) => warnings.push(IndexerItemWarning {
                index,
                code: IndexerItemWarningCode::InvalidItem,
            }),
        }
    }
    if items.is_empty() && !warnings.is_empty() {
        return Err(IndexerError::InvalidResponse);
    }
    Ok((items, warnings))
}
fn element_text(item: Node<'_, '_>, name: &str) -> Option<String> {
    item.children()
        .find(|n| {
            n.is_element() && n.tag_name().name() == name && n.tag_name().namespace().is_none()
        })
        .map(|n| n.children().filter_map(|c| c.text()).collect::<String>())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
fn nzb_item(item: Node<'_, '_>, base: &url::Url, domain: MediaDomain) -> Result<Release> {
    let title = element_text(item, "title").ok_or(IndexerError::InvalidResponse)?;
    let title = html_escape::decode_html_entities(&title).into_owned();
    if !valid_text(&title, TITLE_BYTES) {
        return Err(IndexerError::InvalidResponse);
    }
    let published =
        feed::feed_date(&element_text(item, "pubDate").ok_or(IndexerError::InvalidResponse)?)?;
    let enclosure = item
        .children()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "enclosure"
                && n.tag_name().namespace().is_none()
                && n.attribute("url").is_some_and(|u| !u.trim().is_empty())
                && !n
                    .attribute("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("application/x-bittorrent"))
        })
        .ok_or(IndexerError::InvalidResponse)?;
    let url = base
        .join(enclosure.attribute("url").unwrap_or("").trim())
        .map_err(|_| IndexerError::InvalidResponse)?;
    let download_url = locator(url.as_str(), false)?;
    let size = enclosure
        .attribute("length")
        .map(|n| {
            n.trim()
                .parse::<u64>()
                .map_err(|_| IndexerError::InvalidResponse)
        })
        .transpose()?
        .filter(|n| *n > 0);
    let guid = element_text(item, "guid").unwrap_or_else(|| download_url.clone());
    let info_url = element_text(item, "link")
        .or_else(|| Some(guid.clone()))
        .and_then(|link| locator(&link, false).ok());
    let _ = domain;
    Ok(Release {
        metadata: ReleaseMetadata {
            title: Some(title),
            size_bytes: size,
            published_at: published,
            categories: vec![TV_ANIME],
            seeders: None,
            leechers: None,
            peers: None,
            languages: Vec::new(),
        },
        guid: Some(guid),
        download_url,
        facts: ReleaseFacts {
            identifiers: ReleaseIdentifiers::Tv {
                tvdb_id: None,
                tvmaze_id: None,
                tvrage_id: None,
                tmdb_id: None,
                imdb_id: None,
            },
            scene: None,
            nuked: None,
            subtitles: Vec::new(),
            has_subtitles: None,
            comments_url: None,
            info_url,
            torrent: None,
        },
        attributes: BTreeMap::new(),
    })
}

// ---------------------------------------------------------------------------------------------
// BroadcastheNet (JSON-RPC)
// ---------------------------------------------------------------------------------------------

fn btn_criteria(request: &IndexerSearch) -> Result<Value> {
    let mut criteria = serde_json::Map::new();
    match request {
        IndexerSearch::Rss { .. } => {}
        IndexerSearch::Tv {
            title,
            tvdb_id,
            rage_id,
            numbering,
            ..
        } => {
            match (tvdb_id, rage_id) {
                (Some(id), _) => {
                    criteria.insert("tvdb".into(), json!(id));
                }
                (None, Some(id)) => {
                    criteria.insert("tvrage".into(), json!(id));
                }
                (None, None) => {
                    criteria.insert("series".into(), json!(query_text(title)?));
                }
            }
            match numbering {
                TvNumbering::Episode { season, episode } => {
                    criteria.insert("name".into(), json!(format!("S{season:02}E{episode:02}")));
                    criteria.insert("category".into(), json!("Episode"));
                }
                TvNumbering::Season { season } | TvNumbering::AnimeSeason { season, .. } => {
                    criteria.insert("name".into(), json!(format!("Season {season}")));
                    criteria.insert("category".into(), json!("Season"));
                }
                TvNumbering::Daily { date } => {
                    criteria.insert("name".into(), json!(date.replace('-', ".")));
                    criteria.insert("category".into(), json!("Episode"));
                }
                TvNumbering::Anime {
                    season: Some(season),
                    episode: Some(episode),
                    ..
                } => {
                    criteria.insert("name".into(), json!(format!("S{season:02}E{episode:02}")));
                    criteria.insert("category".into(), json!("Episode"));
                }
                // BroadcastheNet has no absolute numbering: the series alone is searched.
                TvNumbering::Anime { .. }
                | TvNumbering::DailySeason { .. }
                | TvNumbering::Special { .. } => {}
            }
        }
        IndexerSearch::Movie { .. } => return Err(IndexerError::Unsupported),
    }
    Ok(Value::Object(criteria))
}
fn btn_resolution_category(resolution: &str) -> u32 {
    match resolution {
        "SD" | "Portable Device" => 5030,
        "2160p" | "4K" => 5045,
        "" => TV_ROOT,
        _ => 5040,
    }
}
fn parse_btn(body: &[u8], domain: MediaDomain) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let value = json_body(body)?;
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return Err(match error.get("code").and_then(Value::as_i64) {
            Some(-32001) => IndexerError::Authentication,
            Some(-32002) => IndexerError::RateLimited {
                retry_after_seconds: Some(BTN_CALL_LIMIT_SECONDS),
            },
            _ => IndexerError::InvalidResponse,
        });
    }
    let result = value.get("result").ok_or(IndexerError::InvalidResponse)?;
    // PHP serializes an empty torrent map as an empty array.
    let rows: Vec<&Value> = match result.get("torrents") {
        Some(Value::Object(map)) => map.values().collect(),
        Some(Value::Array(list)) if list.is_empty() => Vec::new(),
        None | Some(Value::Null) => Vec::new(),
        Some(_) => return Err(IndexerError::InvalidResponse),
    };
    rows_to_releases(rows.into_iter(), domain, |row| {
        let origin = string(row, "Origin").unwrap_or_default();
        Ok(Row {
            kind: TrackerKind::Broadcasthenet,
            id: string(row, "TorrentID").ok_or(IndexerError::InvalidResponse)?,
            title: string(row, "ReleaseName").ok_or(IndexerError::InvalidResponse)?,
            size: number(row, "Size")?,
            published_at: unix_date(number(row, "Time")?.ok_or(IndexerError::InvalidResponse)?)?,
            seeders: count(row, "Seeders")?,
            leechers: count(row, "Leechers")?,
            download_url: string(row, "DownloadURL").ok_or(IndexerError::InvalidResponse)?,
            info_url: None,
            info_hash: string(row, "InfoHash"),
            imdb: number(row, "ImdbID")?.filter(|n| *n > 0).map(imdb_id),
            tvdb: count(row, "TvdbID")?.filter(|n| *n > 0),
            free: None,
            internal: Some(origin == "Internal"),
            scene: Some(origin == "Scene"),
            category: btn_resolution_category(&string(row, "Resolution").unwrap_or_default()),
        })
    })
}
async fn fetch_btn(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
    fetch_size: u32,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let key = access.api_key.ok_or(IndexerError::Authentication)?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": "hrrdarr",
        "method": "getTorrents",
        "params": [key, btn_criteria(request)?, fetch_size, 0],
    });
    let bytes = post_json(operation, t.endpoint, &[], &body).await?;
    cooldown(operation, parse_btn(&bytes, t.domain))
}

// ---------------------------------------------------------------------------------------------
// FileList (JSON, HTTP basic authentication with the passkey)
// ---------------------------------------------------------------------------------------------

fn filelist_params(t: &Target<'_>, request: &IndexerSearch) -> Result<Vec<(String, String)>> {
    let categories = |ids: &[u32]| ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut params = Vec::new();
    match request {
        IndexerSearch::Rss { .. } => params.push(("action".into(), "latest-torrents".into())),
        IndexerSearch::Tv {
            title, numbering, ..
        } => {
            params.push(("action".into(), "search-torrents".into()));
            params.push(("type".into(), "name".into()));
            params.push(("query".into(), query_text(title)?));
            match numbering {
                TvNumbering::Episode { season, episode } => {
                    params.push(("season".into(), season.to_string()));
                    params.push(("episode".into(), episode.to_string()));
                }
                TvNumbering::Season { season } | TvNumbering::AnimeSeason { season, .. } => {
                    params.push(("season".into(), season.to_string()));
                }
                _ => {}
            }
        }
        IndexerSearch::Movie {
            title,
            year,
            imdb_id,
            ..
        } => {
            params.push(("action".into(), "search-torrents".into()));
            match imdb_id {
                Some(id) => {
                    params.push(("type".into(), "imdb".into()));
                    params.push(("query".into(), imdb_text(id)?));
                }
                None => {
                    params.push(("type".into(), "name".into()));
                    params.push(("query".into(), movie_text(title, *year)?));
                }
            }
        }
    }
    if !t.scope.categories.is_empty() {
        params.push(("category".into(), categories(&t.scope.categories)));
    }
    Ok(params)
}
fn imdb_text(value: &str) -> Result<String> {
    Ok(imdb_id(imdb_digits(value)?))
}
fn parse_filelist(
    body: &[u8],
    domain: MediaDomain,
    origin: &str,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let value = json_body(body)?;
    let rows = match &value {
        Value::Array(rows) => rows,
        Value::Object(map) if map.contains_key("error") => {
            return Err(IndexerError::Authentication);
        }
        _ => return Err(IndexerError::InvalidResponse),
    };
    rows_to_releases(rows.iter(), domain, |row| {
        let id = string(row, "id").ok_or(IndexerError::InvalidResponse)?;
        Ok(Row {
            kind: TrackerKind::Filelist,
            info_url: Some(format!("{origin}/details.php?id={id}")),
            id,
            title: string(row, "name").ok_or(IndexerError::InvalidResponse)?,
            size: number(row, "size")?,
            published_at: bucharest_date(
                &string(row, "upload_date").ok_or(IndexerError::InvalidResponse)?,
            )?,
            seeders: count(row, "seeders")?,
            leechers: count(row, "leechers")?,
            download_url: string(row, "download_link").ok_or(IndexerError::InvalidResponse)?,
            info_hash: None,
            imdb: string(row, "imdb")
                .and_then(|v| imdb_digits(&v).ok())
                .map(imdb_id),
            tvdb: None,
            free: flag(row, "freeleech"),
            internal: flag(row, "internal"),
            scene: None,
            category: match domain {
                MediaDomain::Tv => TV_ROOT,
                MediaDomain::Movies => MOVIE_ROOT,
            },
        })
    })
}
async fn fetch_filelist(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let (user, passkey) = credentials(access)?;
    let basic = format!(
        "Basic {}",
        crate::providers::transmission::base64(format!("{user}:{passkey}").as_bytes())
    );
    let bytes = get(
        operation,
        t.endpoint,
        &filelist_params(t, request)?,
        &[("authorization".to_string(), basic)],
    )
    .await?;
    cooldown(
        operation,
        parse_filelist(&bytes, t.domain, &origin(t.endpoint)?),
    )
}

// ---------------------------------------------------------------------------------------------
// HDBits (JSON POST with the account in the body)
// ---------------------------------------------------------------------------------------------

fn hdbits_body(
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
    fetch_size: u32,
) -> Result<Value> {
    let (user, passkey) = credentials(access)?;
    let mut body = serde_json::Map::new();
    body.insert("username".into(), json!(user));
    body.insert("passkey".into(), json!(passkey));
    body.insert("limit".into(), json!(fetch_size));
    if !t.scope.categories.is_empty() {
        body.insert("category".into(), json!(t.scope.categories));
    }
    match request {
        IndexerSearch::Rss { .. } => {}
        IndexerSearch::Tv {
            title,
            tvdb_id,
            numbering,
            ..
        } => match tvdb_id {
            Some(id) => {
                let mut tvdb = serde_json::Map::new();
                tvdb.insert("id".into(), json!(id));
                match numbering {
                    TvNumbering::Episode { season, episode } => {
                        tvdb.insert("season".into(), json!(season));
                        tvdb.insert("episode".into(), json!(episode));
                    }
                    TvNumbering::Season { season } | TvNumbering::AnimeSeason { season, .. } => {
                        tvdb.insert("season".into(), json!(season));
                    }
                    _ => {}
                }
                body.insert("tvdb".into(), Value::Object(tvdb));
            }
            None => {
                body.insert("search".into(), json!(tv_text(title, numbering)?));
            }
        },
        IndexerSearch::Movie {
            title,
            year,
            imdb_id,
            ..
        } => match imdb_id {
            Some(id) => {
                body.insert("imdb".into(), json!({"id": imdb_digits(id)?}));
            }
            None => {
                body.insert("search".into(), json!(movie_text(title, *year)?));
            }
        },
    }
    Ok(Value::Object(body))
}
fn parse_hdbits(
    body: &[u8],
    domain: MediaDomain,
    origin: &str,
    passkey: &str,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let value = json_body(body)?;
    match value.get("status").and_then(Value::as_i64) {
        Some(0) => {}
        // Missing credentials and failed authentication.
        Some(4 | 5) => return Err(IndexerError::Authentication),
        _ => return Err(IndexerError::InvalidResponse),
    }
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(IndexerError::InvalidResponse)?;
    rows_to_releases(rows.iter(), domain, |row| {
        let id = string(row, "id").ok_or(IndexerError::InvalidResponse)?;
        Ok(Row {
            kind: TrackerKind::Hdbits,
            download_url: format!("{origin}/download.php?id={id}&passkey={passkey}"),
            info_url: Some(format!("{origin}/details.php?id={id}")),
            id,
            title: string(row, "name").ok_or(IndexerError::InvalidResponse)?,
            size: number(row, "size")?,
            published_at: match number(row, "utadded")? {
                Some(seconds) => unix_date(seconds)?,
                None => utc_text_date(&string(row, "added").ok_or(IndexerError::InvalidResponse)?)?,
            },
            seeders: count(row, "seeders")?,
            leechers: count(row, "leechers")?,
            info_hash: string(row, "hash"),
            imdb: row
                .get("imdb")
                .and_then(|v| number(v, "id").ok().flatten())
                .filter(|n| *n > 0)
                .map(imdb_id),
            tvdb: row
                .get("tvdb")
                .and_then(|v| count(v, "id").ok().flatten())
                .filter(|n| *n > 0),
            free: flag(row, "freeleech"),
            // HDBits origin codes: 1 internal, 2 scene.
            internal: number(row, "type_origin")?.map(|n| n == 1),
            scene: number(row, "type_origin")?.map(|n| n == 2),
            category: match domain {
                MediaDomain::Tv => TV_ROOT,
                MediaDomain::Movies => MOVIE_ROOT,
            },
        })
    })
}
async fn fetch_hdbits(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
    fetch_size: u32,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let (_, passkey) = credentials(access)?;
    let bytes = post_json(
        operation,
        t.endpoint,
        &[],
        &hdbits_body(t, access, request, fetch_size)?,
    )
    .await?;
    cooldown(
        operation,
        parse_hdbits(&bytes, t.domain, &origin(t.endpoint)?, passkey),
    )
}

// ---------------------------------------------------------------------------------------------
// PassThePopcorn (JSON, ApiUser / ApiKey headers; movies only)
// ---------------------------------------------------------------------------------------------

fn ptp_params(request: &IndexerSearch) -> Result<Vec<(String, String)>> {
    let mut params = vec![
        ("action".to_string(), "advanced".to_string()),
        ("json".to_string(), "noredirect".to_string()),
        ("grouping".to_string(), "0".to_string()),
        ("order_by".to_string(), "time".to_string()),
        ("order_way".to_string(), "desc".to_string()),
    ];
    match request {
        IndexerSearch::Rss { .. } => {}
        IndexerSearch::Movie {
            title,
            year,
            imdb_id,
            ..
        } => params.push((
            "searchstr".to_string(),
            match imdb_id {
                Some(id) => imdb_text(id)?,
                None => movie_text(title, *year)?,
            },
        )),
        IndexerSearch::Tv { .. } => return Err(IndexerError::Unsupported),
    }
    Ok(params)
}
fn parse_ptp(
    body: &[u8],
    domain: MediaDomain,
    origin: &str,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let value = json_body(body)?;
    if value.get("Result").and_then(Value::as_str) == Some("Error") {
        return Err(IndexerError::Authentication);
    }
    let auth_key = string(&value, "AuthKey");
    let pass_key = string(&value, "PassKey");
    let groups = match value.get("Movies") {
        Some(Value::Array(groups)) => groups.as_slice(),
        None | Some(Value::Null) => &[],
        Some(_) => return Err(IndexerError::InvalidResponse),
    };
    // Flatten the group -> torrents hierarchy, carrying the group beside each torrent.
    let mut flat: Vec<(&Value, &Value)> = Vec::new();
    for group in groups {
        match group.get("Torrents") {
            Some(Value::Array(torrents)) => flat.extend(torrents.iter().map(|t| (group, t))),
            None | Some(Value::Null) => {}
            Some(_) => return Err(IndexerError::InvalidResponse),
        }
    }
    rows_to_releases(flat.into_iter(), domain, |(group, row)| {
        let id = string(row, "Id").ok_or(IndexerError::InvalidResponse)?;
        let title = string(row, "ReleaseName")
            .or_else(|| {
                let name = string(group, "Title")?;
                let mut parts = vec![name, string(group, "Year").unwrap_or_default()];
                parts.extend(
                    ["Resolution", "Source", "Codec"].map(|k| string(row, k).unwrap_or_default()),
                );
                Some(
                    parts
                        .into_iter()
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .ok_or(IndexerError::InvalidResponse)?;
        let download_url = format!(
            "{origin}/torrents.php?action=download&id={id}&authkey={}&torrent_pass={}",
            auth_key.as_deref().ok_or(IndexerError::InvalidResponse)?,
            pass_key.as_deref().ok_or(IndexerError::InvalidResponse)?
        );
        Ok(Row {
            kind: TrackerKind::Passthepopcorn,
            info_url: Some(format!(
                "{origin}/torrents.php?id={}&torrentid={id}",
                string(group, "GroupId").unwrap_or_default()
            )),
            id,
            title,
            size: number(row, "Size")?,
            published_at: utc_text_date(
                &string(row, "UploadTime").ok_or(IndexerError::InvalidResponse)?,
            )?,
            seeders: count(row, "Seeders")?,
            leechers: count(row, "Leechers")?,
            download_url,
            info_hash: string(row, "InfoHash"),
            imdb: string(group, "ImdbId")
                .and_then(|v| imdb_digits(&v).ok())
                .map(imdb_id),
            tvdb: None,
            free: match string(row, "FreeleechType").as_deref() {
                Some("Freeleech") => Some(true),
                _ => None,
            },
            internal: None,
            scene: flag(row, "Scene"),
            category: MOVIE_ROOT,
        })
    })
}
async fn fetch_ptp(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let (user, key) = credentials(access)?;
    let bytes = get(
        operation,
        t.endpoint,
        &ptp_params(request)?,
        &[
            ("apiuser".to_string(), user.to_string()),
            ("apikey".to_string(), key.to_string()),
        ],
    )
    .await?;
    cooldown(operation, parse_ptp(&bytes, t.domain, &origin(t.endpoint)?))
}

// ---------------------------------------------------------------------------------------------
// TorrentPotato (JSON, user + passkey query parameters; movies only)
// ---------------------------------------------------------------------------------------------

fn parse_potato(
    body: &[u8],
    domain: MediaDomain,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let value = json_body(body)?;
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return Err(IndexerError::Authentication);
    }
    let rows = match value.get("results") {
        Some(Value::Array(rows)) => rows.as_slice(),
        None | Some(Value::Null) => &[],
        Some(_) => return Err(IndexerError::InvalidResponse),
    };
    rows_to_releases(rows.iter(), domain, |row| {
        // The TorrentPotato size field is in megabytes.
        let size = number(row, "size")?
            .map(|mb| {
                mb.checked_mul(1024 * 1024)
                    .ok_or(IndexerError::InvalidResponse)
            })
            .transpose()?;
        Ok(Row {
            kind: TrackerKind::Torrentpotato,
            id: string(row, "torrent_id")
                .or_else(|| string(row, "release_name"))
                .ok_or(IndexerError::InvalidResponse)?,
            title: string(row, "release_name").ok_or(IndexerError::InvalidResponse)?,
            size,
            published_at: feed::feed_date(
                &string(row, "publish_date").ok_or(IndexerError::InvalidResponse)?,
            )?,
            seeders: count(row, "seeders")?,
            leechers: count(row, "leechers")?,
            download_url: string(row, "download_url").ok_or(IndexerError::InvalidResponse)?,
            info_url: string(row, "details_url"),
            info_hash: None,
            imdb: string(row, "imdb_id")
                .and_then(|v| imdb_digits(&v).ok())
                .map(imdb_id),
            tvdb: None,
            free: flag(row, "freeleech"),
            internal: None,
            scene: None,
            category: MOVIE_ROOT,
        })
    })
}
async fn fetch_potato(
    operation: &HttpOperation<'_>,
    t: &Target<'_>,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<(Vec<Release>, Vec<IndexerItemWarning>)> {
    let (user, passkey) = credentials(access)?;
    let mut params = vec![
        ("user".to_string(), user.to_string()),
        ("passkey".to_string(), passkey.to_string()),
    ];
    match request {
        IndexerSearch::Rss { .. } => {}
        IndexerSearch::Movie {
            title,
            year,
            imdb_id,
            ..
        } => params.push((
            "search".to_string(),
            match imdb_id {
                Some(id) => imdb_text(id)?,
                None => movie_text(title, *year)?,
            },
        )),
        IndexerSearch::Tv { .. } => return Err(IndexerError::Unsupported),
    }
    let bytes = get(operation, t.endpoint, &params, &[]).await?;
    cooldown(operation, parse_potato(&bytes, t.domain))
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

pub(super) async fn fetch(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<Fetched> {
    if !access.validate_tracker() {
        return Err(IndexerError::InvalidRequest);
    }
    let t = target(settings, request_domain(request))?;
    let (offset, query_index, limit) = request_paging(request);
    if limit == 0 || limit > MAX_ITEMS || query_index != 0 {
        return Err(IndexerError::InvalidRequest);
    }
    // Fetch one row more than the page needs so the local pager can tell whether more exist.
    let fetch_size = offset
        .saturating_add(limit)
        .saturating_add(1)
        .clamp(2, FETCH_CEILING);
    if matches!(
        request,
        IndexerSearch::Tv { .. } | IndexerSearch::Movie { .. }
    ) && !t.kind.searchable()
    {
        return Err(IndexerError::Unsupported);
    }
    let (mut items, warnings) = match t.kind {
        TrackerKind::Nyaa
        | TrackerKind::Fanzub
        | TrackerKind::Iptorrents
        | TrackerKind::Torrentleech => fetch_rss(operation, &t, access, request).await?,
        TrackerKind::Broadcasthenet => {
            fetch_btn(operation, &t, access, request, fetch_size).await?
        }
        TrackerKind::Filelist => fetch_filelist(operation, &t, access, request).await?,
        TrackerKind::Hdbits => fetch_hdbits(operation, &t, access, request, fetch_size).await?,
        TrackerKind::Passthepopcorn => fetch_ptp(operation, &t, access, request).await?,
        TrackerKind::Torrentpotato => fetch_potato(operation, &t, access, request).await?,
    };
    let before = items.len();
    if let Some(minimum) = t.scope.minimum_seeders.filter(|n| *n > 0) {
        items.retain(|release| release.metadata.seeders.is_none_or(|n| n >= minimum));
    }
    Ok(Fetched {
        below_minimum_seeders: (before - items.len()) as u32,
        items,
        warnings,
    })
}

/// One local page over the single fetched document, redacted of every credential value.
pub(super) async fn raw_search(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<RawIndexerPage> {
    let fetched = fetch(operation, settings, access, request).await?;
    let (offset, _, limit) = request_paging(request);
    let mut items = fetched.items;
    let total = items.len() as u32;
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    items = items.drain(start as usize..end as usize).collect();
    redact_secrets(&mut items, access);
    Ok(RawIndexerPage {
        tolerates_item_warnings: true,
        warnings: if start == 0 {
            fetched.warnings
        } else {
            Vec::new()
        },
        query_index: 0,
        query_count: 1,
        next_query: (end < total).then_some(IndexerContinuation {
            query_index: 0,
            offset: end,
        }),
        media_type: request_domain(request),
        items,
        offset: start,
        limit,
        total: Some(total),
        next_offset: (end < total).then_some(end),
    })
}

/// Fetches and parses every configured scope. An empty successful document is valid evidence of
/// access, not of content. The failing domain is reported with the error so backoff stays scoped.
pub(super) async fn test(
    operation: &HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
) -> std::result::Result<FeedTest, (IndexerError, Option<MediaDomain>)> {
    let (_, tv, movies) = settings
        .tracker_parts()
        .ok_or((IndexerError::Unsupported, None))?;
    let mut domains = Vec::new();
    for domain in tv
        .as_ref()
        .map(|_| MediaDomain::Tv)
        .into_iter()
        .chain(movies.as_ref().map(|_| MediaDomain::Movies))
    {
        let fetched = fetch(
            operation,
            settings,
            access,
            &IndexerSearch::Rss {
                media_type: domain,
                offset: 0,
                query_index: 0,
                limit: 1,
            },
        )
        .await
        .map_err(|error| (error, Some(domain)))?;
        domains.push(FeedDomainTest {
            media_type: domain,
            parsed: (fetched.items.len() as u32) + fetched.below_minimum_seeders,
            rejected: fetched.warnings.len() as u32,
            below_minimum_seeders: fetched.below_minimum_seeders,
        });
    }
    Ok(FeedTest { domains })
}

#[cfg(test)]
mod tests;
