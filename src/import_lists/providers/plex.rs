//! Plex watchlist (Sonarr/Radarr "Plex Watchlist"): the account's cloud watchlist.
//!
//! `GET https://metadata.provider.plex.tv/library/sections/watchlist/all` with the account token in
//! the `X-Plex-Token` header (never in the URL), paged by `X-Plex-Container-Start`. A TV list keeps
//! `show` rows (TVDB id, IMDb id), a movie list keeps `movie` rows (TMDb id, IMDb id); rows of other
//! types are skipped. GUID schemes other than imdb/tmdb/tvdb are ignored, but a recognised GUID with
//! a malformed id fails the whole fetch.
use super::*;
use crate::import_lists::model::ListItem;

const ORIGIN: &str = "https://metadata.provider.plex.tv";
const PATH: &str = "/library/sections/watchlist/all";
const PAGE_SIZE: usize = 100;
const CLIENT_ID: &str = "hrrdarr";

pub fn implementation() -> Implementation {
    Implementation {
        name: "plex",
        label: "Plex watchlist",
        description: "The Plex account watchlist (shows for TV lists, movies for movie lists), read with the account's Plex token.",
        domains: &[MediaDomain::Tv, MediaDomain::Movies],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "token",
            "Plex token",
            FieldKind::Text,
            "The account's X-Plex-Token; sent only as a header.",
        )
        .required()
        .secret(),
    ]
}

/// Splits `scheme://value`; anything without that shape is not a GUID we understand.
fn split_guid(raw: &str) -> Option<(&str, &str)> {
    raw.split_once("://")
}

fn parse_row(media: MediaDomain, row: &Value) -> Result<Option<ListItem>, FetchError> {
    let object = row.as_object().ok_or(FetchError::InvalidResponse)?;
    let wanted = match media {
        MediaDomain::Tv => "show",
        MediaDomain::Movies => "movie",
    };
    if object.get("type").and_then(Value::as_str) != Some(wanted) {
        return Ok(None);
    }
    let (mut tvdb, mut tmdb, mut imdb) = (None, None, None);
    match object.get("Guid") {
        None | Some(Value::Null) => {}
        Some(Value::Array(guids)) => {
            for guid in guids {
                let raw = guid
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(FetchError::InvalidResponse)?;
                let Some((scheme, value)) = split_guid(raw) else {
                    continue;
                };
                let value = Value::String(value.to_owned());
                match scheme {
                    "tvdb" => tvdb = id_of(Some(&value))?,
                    "tmdb" => tmdb = id_of(Some(&value))?,
                    "imdb" => imdb = imdb_of(Some(&value))?,
                    _ => {}
                }
            }
        }
        Some(_) => return Err(FetchError::InvalidResponse),
    }
    Ok(Some(ListItem {
        tvdb_id: if media == MediaDomain::Tv { tvdb } else { None },
        tmdb_id: if media == MediaDomain::Movies {
            tmdb
        } else {
            None
        },
        imdb_id: imdb,
        title: title_of(object.get("title")),
        year: year_of(object.get("year"))?,
    }))
}

/// One page: its rows (already filtered to the list's domain) and the raw row count, plus the
/// declared `totalSize` when present.
pub struct Page {
    pub items: Vec<ListItem>,
    pub rows: usize,
    pub total: Option<i64>,
}
pub fn parse_page(media: MediaDomain, document: &Value) -> Result<Page, FetchError> {
    let container = document
        .get("MediaContainer")
        .and_then(Value::as_object)
        .ok_or(FetchError::InvalidResponse)?;
    let total = match container.get("totalSize") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_i64()
                .filter(|n| *n >= 0)
                .ok_or(FetchError::InvalidResponse)?,
        ),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let rows = match container.get("Metadata") {
        None | Some(Value::Null) => &Vec::new(),
        Some(Value::Array(rows)) => rows,
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let mut items = Vec::new();
    for row in rows {
        if let Some(item) = parse_row(media, row)? {
            items.push(item);
        }
    }
    Ok(Page {
        items,
        rows: rows.len(),
        total,
    })
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let token = list.need_secret("token")?;
    let headers = [
        ("x-plex-token", token),
        ("x-plex-client-identifier", CLIENT_ID),
        ("x-plex-product", CLIENT_ID),
    ];
    let mut all = Vec::new();
    let mut start = 0usize;
    for _ in 0..MAX_REQUESTS {
        let url = format!(
            "{ORIGIN}{PATH}?includeGuids=1&X-Plex-Container-Start={start}&X-Plex-Container-Size={PAGE_SIZE}"
        );
        let page = parse_page(list.media, &get_json(transport, url, &headers).await?)?;
        all.extend(page.items);
        if all.len() > MAX_ITEMS * 2 {
            return Err(FetchError::ResourceLimit);
        }
        start += page.rows;
        let done = page.rows == 0 || page.total.is_some_and(|t| start as i64 >= t);
        if done {
            return finish(list.media, all);
        }
    }
    Err(FetchError::ResourceLimit)
}
