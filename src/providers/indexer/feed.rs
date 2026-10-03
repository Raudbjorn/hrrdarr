//! Generic RSS 2.0 torrent feeds (the `torrentrss` implementation).
//!
//! A plain feed has no capability document and no search API: it can only be polled for RSS.
//! The parser is written from RSS 2.0 and common torrent-feed conventions (`enclosure`, `link`,
//! `guid`, `pubDate`, the ezrss `torrent:` and Nyaa `nyaa:` namespaces and Torznab `attr`
//! elements). It is deliberately independent of the reference applications and generic over
//! namespaces so other feed-style indexers can reuse it.
//!
//! Failure policy (fixed, tested):
//! - The feed is invalid (`InvalidResponse`) when it is not well-formed XML, carries a DTD or
//!   entity declaration, is larger than the transport/XML caps, is not `rss`/`channel`, has more
//!   than [`FEED_MAX_ITEMS`] items, or when it has items but *none* of them is valid.
//! - Otherwise a malformed item is dropped on its own and reported as an item warning; the valid
//!   items are still returned. Duplicate `guid`s keep the first item and warn for the rest.
use super::{
    IndexerError, IndexerItemWarning, IndexerItemWarningCode, Release, ReleaseFacts,
    ReleaseIdentifiers, ReleaseMetadata, Result, TorrentFacts, attributes, boolean, child, decimal,
    identifier, locator, optional_text, scalar, selected_magnet, text, valid_text, xml,
};
use crate::api::MediaDomain;
use crate::providers::{IndexerAccess, ProviderSettings, http::HttpRequestBody};
use roxmltree::Node;
use serde::Serialize;
use std::collections::BTreeSet;

/// Hard bound on items per feed document; larger feeds are rejected, not truncated.
pub const FEED_MAX_ITEMS: usize = 500;
const MAX_ENCLOSURES: usize = 16;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const EZRSS: &str = "http://xmlns.ezrss.it/0.1/";
const NYAA: &str = "https://nyaa.si/xmlns/nyaa";
const DUBLIN_CORE: &str = "http://purl.org/dc/elements/1.1/";

#[derive(Serialize, ts_rs::TS)]
pub struct FeedDomainTest {
    pub media_type: MediaDomain,
    // Valid, distinct items in the feed document.
    pub parsed: u32,
    // Items dropped individually as malformed or duplicate.
    pub rejected: u32,
    // Valid items skipped because they report fewer seeders than the scope minimum.
    pub below_minimum_seeders: u32,
}
#[derive(Serialize, ts_rs::TS)]
pub struct FeedTest {
    pub domains: Vec<FeedDomainTest>,
}

pub(super) struct ParsedFeed {
    pub items: Vec<Release>,
    pub warnings: Vec<IndexerItemWarning>,
}

/// An element in `namespaces` (or in no namespace when `namespaces` contains the empty string).
fn field<'a, 'i>(
    item: Node<'a, 'i>,
    namespaces: &[&str],
    name: &str,
) -> Result<Option<Node<'a, 'i>>> {
    let mut found = item.children().filter(|n| {
        n.is_element()
            && n.tag_name().name().eq_ignore_ascii_case(name)
            && namespaces.contains(&n.tag_name().namespace().unwrap_or(""))
    });
    let first = found.next();
    if found.next().is_some() {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(first)
}
fn field_text(item: Node<'_, '_>, namespaces: &[&str], name: &str) -> Result<Option<String>> {
    let Some(node) = field(item, namespaces, name)? else {
        return Ok(None);
    };
    if node.children().any(|n| n.is_element()) {
        return Err(IndexerError::InvalidResponse);
    }
    let value = node.children().filter_map(|n| n.text()).collect::<String>();
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if !valid_text(value, 8192) {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(Some(value.to_string()))
}
fn first_text(item: Node<'_, '_>, candidates: &[(&[&str], &str)]) -> Result<Option<String>> {
    for (namespaces, name) in candidates {
        if let Some(value) = field_text(item, namespaces, name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
fn count(value: &str) -> Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| IndexerError::InvalidResponse)
}

/// Bytes from a plain integer or a decimal with a binary/decimal unit suffix. Both `KB` and `KiB`
/// are 1024-based: torrent trackers use the suffixes interchangeably.
fn byte_size(value: &str) -> Result<u64> {
    let value = value.trim();
    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" | "bytes" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(IndexerError::InvalidResponse),
    };
    if number.is_empty() || number.matches('.').count() > 1 || number.starts_with('.') {
        return Err(IndexerError::InvalidResponse);
    }
    let bytes = if multiplier == 1.0 {
        number
            .parse::<u64>()
            .map_err(|_| IndexerError::InvalidResponse)?
    } else {
        let n = number
            .parse::<f64>()
            .map_err(|_| IndexerError::InvalidResponse)?;
        let bytes = n * multiplier;
        if !bytes.is_finite() || bytes >= MAX_SAFE_INTEGER as f64 {
            return Err(IndexerError::InvalidResponse);
        }
        bytes as u64
    };
    if bytes > MAX_SAFE_INTEGER {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(bytes)
}

/// Absolute publication instants only. Zone-less values are taken as UTC; relative phrases such as
/// "2 hours ago" are rejected rather than guessed, so a replay can never move a release's age.
pub(super) fn feed_date(value: &str) -> Result<String> {
    let value = value.trim();
    let parsed = chrono::DateTime::parse_from_rfc2822(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(value))
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| {
            [
                "%Y-%m-%d %H:%M:%S",
                "%a, %d %b %Y %H:%M:%S",
                "%Y-%m-%dT%H:%M:%S",
            ]
            .iter()
            .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())
            .map(|d| d.and_utc())
            .or_else(|| {
                chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|d| d.and_utc())
            })
            .ok_or(IndexerError::InvalidResponse)
        })?;
    Ok(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn resolve(base: &url::Url, value: &str) -> Result<String> {
    if value.len() > 8192 {
        return Err(IndexerError::InvalidResponse);
    }
    // Relative references resolve against the feed URL (never against private query parameters).
    let joined = base
        .join(value.trim())
        .map_err(|_| IndexerError::InvalidResponse)?;
    locator(joined.as_str(), true)
}

fn parse_item(item: Node<'_, '_>, base: &url::Url, domain: MediaDomain) -> Result<Release> {
    let none: &[&str] = &[""];
    let ezrss: &[&str] = &[EZRSS];
    let nyaa: &[&str] = &[NYAA];
    let dc: &[&str] = &[DUBLIN_CORE];
    let title = text(item, "title", 8192)?;
    let title = html_escape::decode_html_entities(title.trim()).into_owned();
    if !valid_text(&title, 2048) {
        return Err(IndexerError::InvalidResponse);
    }
    let guid = optional_text(item, "guid", 8192)?;
    let attrs = attributes(item)?;
    let published_at = feed_date(
        &first_text(item, &[(none, "pubDate"), (dc, "date")])?
            .ok_or(IndexerError::InvalidResponse)?,
    )?;

    // Enclosures: a torrent-typed one wins, then the first untyped/unknown one. An NZB enclosure
    // is not a torrent and is never selected.
    let mut typed = None;
    let mut untyped = None;
    for (index, node) in item
        .children()
        .filter(|n| {
            n.is_element()
                && n.tag_name().name() == "enclosure"
                && n.tag_name().namespace().is_none()
        })
        .enumerate()
    {
        if index >= MAX_ENCLOSURES {
            return Err(IndexerError::InvalidResponse);
        }
        if node.attribute("url").is_none() {
            continue;
        }
        let mime = node
            .attribute("type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if mime.eq_ignore_ascii_case("application/x-bittorrent") {
            typed.get_or_insert(node);
        } else if !mime.eq_ignore_ascii_case("application/x-nzb") {
            untyped.get_or_insert(node);
        }
    }
    let enclosure = typed.or(untyped);

    let magnet = match scalar(&attrs, "magneturl")? {
        Some(value) => Some(value.to_string()),
        None => field_text(item, ezrss, "magnetURI")?,
    }
    .map(|value| locator(&value, true))
    .transpose()?;
    let download_url = if let Some(url) = enclosure.and_then(|n| n.attribute("url")) {
        resolve(base, url)?
    } else if let Some(magnet) = &magnet {
        magnet.clone()
    } else {
        let link = field_text(item, none, "link")?.ok_or(IndexerError::InvalidResponse)?;
        resolve(base, &link)?
    };

    // Size: explicit attributes first; an enclosure length of zero means "not reported".
    let size_text = first_text(
        item,
        &[(ezrss, "contentLength"), (nyaa, "size"), (none, "size")],
    )?;
    let attr_size = scalar(&attrs, "size")?.map(byte_size).transpose()?;
    let enclosure_size = enclosure
        .and_then(|n| n.attribute("length"))
        .map(byte_size)
        .transpose()?;
    let size_bytes = attr_size
        .or(size_text.as_deref().map(byte_size).transpose()?)
        .or(enclosure_size)
        .filter(|n| *n > 0);

    let seeders = match scalar(&attrs, "seeders")? {
        Some(v) => Some(count(v)?),
        None => first_text(
            item,
            &[(ezrss, "seeds"), (nyaa, "seeders"), (none, "seeders")],
        )?
        .as_deref()
        .map(count)
        .transpose()?,
    };
    let explicit_leechers = match scalar(&attrs, "leechers")? {
        Some(v) => Some(count(v)?),
        None => first_text(item, &[(nyaa, "leechers"), (none, "leechers")])?
            .as_deref()
            .map(count)
            .transpose()?,
    };
    let peers = match scalar(&attrs, "peers")? {
        Some(v) => Some(count(v)?),
        None => match first_text(item, &[(ezrss, "peers"), (none, "peers")])? {
            Some(v) => Some(count(&v)?),
            None => seeders
                .zip(explicit_leechers)
                .map(|(s, l)| s.checked_add(l).ok_or(IndexerError::InvalidResponse))
                .transpose()?,
        },
    };
    let leechers =
        explicit_leechers.or_else(|| peers.zip(seeders).and_then(|(p, s)| p.checked_sub(s)));

    let info_hash = match scalar(&attrs, "infohash")? {
        Some(v) => Some(v.to_string()),
        None => first_text(item, &[(ezrss, "infoHash"), (nyaa, "infoHash")])?,
    }
    .map(|hash| {
        if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(hash.to_ascii_lowercase())
        } else {
            Err(IndexerError::InvalidResponse)
        }
    })
    .transpose()?
    .or_else(|| {
        // A 40-hex BitTorrent v1 hash can be read from the magnet when no hash was reported.
        let magnet = magnet.as_deref().or(Some(download_url.as_str()))?;
        let url = url::Url::parse(magnet).ok()?;
        if url.scheme() != "magnet" {
            return None;
        }
        url.query_pairs()
            .find(|(k, _)| k == "xt")
            .and_then(|(_, v)| v.strip_prefix("urn:btih:").map(str::to_owned))
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|h| h.to_ascii_lowercase())
    });

    // Feeds have no category vocabulary, so the scope the item was fetched for is its domain: the
    // standard root category (TV 5000, Movies 2000) satisfies the decision engine's media-category
    // guard, while title parsing and library matching still decide. Explicit numeric Torznab
    // categories on an item win, so a feed can still be rejected for the wrong domain.
    let mut categories = attrs
        .get("category")
        .into_iter()
        .flatten()
        .map(|value| super::category_id(Some(value)))
        .collect::<Result<Vec<_>>>()?;
    if categories.is_empty() {
        categories.push(match domain {
            MediaDomain::Tv => 5000,
            MediaDomain::Movies => 2000,
        });
    }
    let comments_url =
        field_text(item, none, "comments")?.and_then(|value| locator(&value, false).ok());
    let info_url = comments_url
        .as_ref()
        .map(|url| url.strip_suffix("#comments").unwrap_or(url).to_string());

    let imdb_id = scalar(&attrs, "imdb")?
        .and_then(|s| s.strip_prefix("tt").unwrap_or(s).parse::<u64>().ok())
        .filter(|n| *n > 0 && *n <= 9_999_999_999)
        .map(|n| format!("tt{n:07}"));
    let tmdb_id = identifier(&attrs, "tmdbid")?;
    let identifiers = match domain {
        MediaDomain::Tv => ReleaseIdentifiers::Tv {
            tvdb_id: identifier(&attrs, "tvdbid")?,
            tvmaze_id: identifier(&attrs, "tvmazeid")?,
            tvrage_id: identifier(&attrs, "rageid")?,
            tmdb_id,
            imdb_id,
        },
        MediaDomain::Movies => ReleaseIdentifiers::Movie { tmdb_id, imdb_id },
    };
    let minimum_seed_seconds = scalar(&attrs, "minimumseedtime")?
        .map(|s| s.parse::<u64>().map_err(|_| IndexerError::InvalidResponse))
        .transpose()?;
    let torrent = TorrentFacts {
        info_hash,
        magnet_url: match magnet {
            Some(value) => Some(value),
            None => selected_magnet(&download_url)?,
        },
        download_volume_factor: decimal(&attrs, "downloadvolumefactor")?,
        upload_volume_factor: decimal(&attrs, "uploadvolumefactor")?,
        minimum_ratio: decimal(&attrs, "minimumratio")?,
        minimum_seed_seconds,
        internal: boolean(&attrs, "internal")?,
    };
    Ok(Release {
        metadata: ReleaseMetadata {
            title: Some(title),
            size_bytes,
            published_at,
            categories,
            seeders,
            leechers,
            peers,
            languages: Vec::new(),
        },
        guid,
        download_url,
        facts: ReleaseFacts {
            identifiers,
            scene: None,
            nuked: None,
            subtitles: Vec::new(),
            has_subtitles: None,
            comments_url,
            info_url,
            torrent: Some(torrent),
        },
        attributes: attrs,
    })
}

pub(super) fn parse_feed(input: &str, base: &url::Url, domain: MediaDomain) -> Result<ParsedFeed> {
    let doc = xml(input.strip_prefix('\u{feff}').unwrap_or(input))?;
    let root = doc.root_element();
    if root.tag_name().name() != "rss" || root.tag_name().namespace().is_some() {
        return Err(IndexerError::InvalidResponse);
    }
    let channel = child(root, "channel")?.ok_or(IndexerError::InvalidResponse)?;
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut guids = BTreeSet::new();
    let mut raw = 0usize;
    for item in channel.children().filter(|n| n.has_tag_name("item")) {
        if raw >= FEED_MAX_ITEMS {
            return Err(IndexerError::InvalidResponse);
        }
        let index = raw as u32;
        raw += 1;
        match parse_item(item, base, domain) {
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
    Ok(ParsedFeed { items, warnings })
}

/// Domain scope and secret parameters for one fetch of the configured feed.
fn scope<'a>(
    settings: &'a ProviderSettings,
    domain: MediaDomain,
) -> Result<(&'a str, &'a crate::providers::FeedScope)> {
    let ProviderSettings::Torrentrss {
        endpoint,
        tv,
        movies,
    } = settings
    else {
        return Err(IndexerError::Unsupported);
    };
    let scope = match domain {
        MediaDomain::Tv => tv.as_ref(),
        MediaDomain::Movies => movies.as_ref(),
    };
    scope
        .map(|scope| (endpoint.as_str(), scope))
        .ok_or(IndexerError::Unsupported)
}

pub(super) struct FetchedFeed {
    pub feed: ParsedFeed,
    pub below_minimum_seeders: u32,
}

pub(super) async fn fetch(
    operation: &crate::providers::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
    domain: MediaDomain,
) -> Result<FetchedFeed> {
    if !access.validate_feed() {
        return Err(IndexerError::InvalidRequest);
    }
    let (endpoint, scope) = scope(settings, domain)?;
    let base = url::Url::parse(endpoint).map_err(|_| IndexerError::InvalidRequest)?;
    let parameters: Vec<(String, String)> = match domain {
        MediaDomain::Tv => access.tv_parameters,
        MediaDomain::Movies => access.movie_parameters,
    }
    .iter()
    .map(|p| (p.name.clone(), p.value.clone()))
    .collect();
    let headers: Vec<(String, String)> = access
        .cookie
        .map(|cookie| vec![("cookie".to_string(), cookie.to_string())])
        .unwrap_or_default();
    let response = operation
        .request(endpoint, &parameters, &headers, HttpRequestBody::Empty)
        .await?;
    if !(200..300).contains(&response.status) {
        return Err(IndexerError::Transport(
            crate::providers::http::HttpError::Transport,
        ));
    }
    let body = String::from_utf8(response.body).map_err(|_| IndexerError::InvalidResponse)?;
    let mut feed = super::cooldown(operation, parse_feed(&body, &base, domain))?;
    let before = feed.items.len();
    if let Some(minimum) = scope.minimum_seeders.filter(|n| *n > 0) {
        feed.items
            .retain(|release| release.metadata.seeders.is_none_or(|n| n >= minimum));
    }
    let below_minimum_seeders = (before - feed.items.len()) as u32;
    Ok(FetchedFeed {
        feed,
        below_minimum_seeders,
    })
}

/// Fetches and parses every configured scope. Empty successful feeds are valid evidence of
/// access, not of content; any failing scope fails the test.
pub async fn test(
    operation: &crate::providers::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
) -> Result<FeedTest> {
    let ProviderSettings::Torrentrss { tv, movies, .. } = settings else {
        return Err(IndexerError::Unsupported);
    };
    let mut domains = Vec::new();
    for domain in tv
        .as_ref()
        .map(|_| MediaDomain::Tv)
        .into_iter()
        .chain(movies.as_ref().map(|_| MediaDomain::Movies))
    {
        let fetched = fetch(operation, settings, access, domain).await?;
        domains.push(FeedDomainTest {
            media_type: domain,
            parsed: (fetched.feed.items.len() as u32) + fetched.below_minimum_seeders,
            rejected: fetched.feed.warnings.len() as u32,
            below_minimum_seeders: fetched.below_minimum_seeders,
        });
    }
    Ok(FeedTest { domains })
}

#[cfg(test)]
mod tests;
