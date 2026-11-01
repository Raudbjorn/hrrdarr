//! TMDb movie lists (Radarr "TMDb Popular/List/Person/Company/Keyword") over the TMDb v3 API.
//!
//! Authentication is a TMDb v4 "API Read Access Token" sent as `Authorization: Bearer` (never in the
//! URL). Pages are requested with `page`; fetching stops at `limit` items, at `total_pages`, or on an
//! empty page. A list needing more than [`MAX_REQUESTS`] requests before reaching the limit fails with
//! `resource_limit`. Items carry the TMDb id, title and the year of `release_date` (absent or empty
//! dates give no year).
use super::*;
use crate::import_lists::model::{ListItem, SettingValue};

const ORIGIN: &str = "https://api.themoviedb.org";
const LIMIT_DEFAULT: i64 = 200;
/// 25 requests of 20 results: a larger limit could never be satisfied within the request budget.
const LIMIT_MAX: i64 = 500;
const MAX_SAFE: i64 = 9_007_199_254_740_991;

pub fn implementation() -> Implementation {
    Implementation {
        name: "tmdb",
        label: "TMDb",
        description: "Popular, top rated, upcoming or now-playing movies, a TMDb list, or movies by person, company or keyword. \
            Needs a TMDb v4 read access token.",
        domains: &[MediaDomain::Movies],
        default_interval_minutes: 360,
        min_interval_minutes: 15,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "list_type",
            "List type",
            FieldKind::Select,
            "Which TMDb collection to import.",
        )
        .options(&[
            "popular",
            "top_rated",
            "upcoming",
            "now_playing",
            "list",
            "person",
            "company",
            "keyword",
        ])
        .with_default(text_default("popular")),
        field(
            "target_id",
            "Target ID",
            FieldKind::Number,
            "TMDb list, person, company or keyword id; required for those list types.",
        )
        .range(1, MAX_SAFE),
        field(
            "limit",
            "Item limit",
            FieldKind::Number,
            "Total movies to import (20 per page).",
        )
        .range(1, LIMIT_MAX)
        .with_default(SettingValue::Int(LIMIT_DEFAULT)),
        field(
            "access_token",
            "Read access token",
            FieldKind::Text,
            "TMDb v4 API read access token; sent as a bearer token.",
        )
        .required()
        .secret(),
    ]
}

/// Request path and fixed query for the list type, without the `page` parameter.
fn endpoint(list: &ListRuntime, list_type: &str) -> Result<(String, String), FetchError> {
    let target = || match list.int("target_id") {
        Some(id) if (1..=MAX_SAFE).contains(&id) => Ok(id),
        _ => Err(FetchError::InvalidSettings),
    };
    Ok(match list_type {
        "popular" | "top_rated" | "upcoming" | "now_playing" => {
            (format!("/3/movie/{list_type}"), String::new())
        }
        "list" => (format!("/3/list/{}", target()?), String::new()),
        "person" => (
            "/3/discover/movie".into(),
            format!("with_cast={}&", target()?),
        ),
        "company" => (
            "/3/discover/movie".into(),
            format!("with_companies={}&", target()?),
        ),
        "keyword" => (
            "/3/discover/movie".into(),
            format!("with_keywords={}&", target()?),
        ),
        _ => return Err(FetchError::InvalidSettings),
    })
}

fn year_from_date(value: Option<&Value>) -> Result<Option<i64>, FetchError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let year = s.get(..4).filter(|y| y.bytes().all(|b| b.is_ascii_digit()));
            match year.and_then(|y| y.parse::<i64>().ok()) {
                Some(y) if (1..=9999).contains(&y) => Ok(Some(y)),
                _ => Err(FetchError::InvalidResponse),
            }
        }
        Some(_) => Err(FetchError::InvalidResponse),
    }
}

/// One page: the movies, `total_pages` when present, and the raw entry count.
pub fn parse_page(document: &Value) -> Result<(Vec<ListItem>, Option<i64>, usize), FetchError> {
    let object = document.as_object().ok_or(FetchError::InvalidResponse)?;
    let entries = object
        .get("results")
        .or_else(|| object.get("items"))
        .and_then(Value::as_array)
        .ok_or(FetchError::InvalidResponse)?;
    let total_pages = match object.get("total_pages") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_i64()
                .filter(|n| *n >= 0)
                .ok_or(FetchError::InvalidResponse)?,
        ),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = entry.as_object().ok_or(FetchError::InvalidResponse)?;
        match entry.get("media_type") {
            None | Some(Value::Null) => {}
            Some(Value::String(kind)) if kind == "movie" => {}
            Some(Value::String(_)) => continue,
            Some(_) => return Err(FetchError::InvalidResponse),
        }
        items.push(ListItem {
            tvdb_id: None,
            tmdb_id: id_of(entry.get("id"))?,
            imdb_id: None,
            title: title_of(entry.get("title")),
            year: year_from_date(entry.get("release_date"))?,
        });
    }
    Ok((items, total_pages, entries.len()))
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let token = list.need_secret("access_token")?;
    let limit = list.int("limit").unwrap_or(LIMIT_DEFAULT);
    if !(1..=LIMIT_MAX).contains(&limit) {
        return Err(FetchError::InvalidSettings);
    }
    let limit = limit as usize;
    let (path, fixed) = endpoint(list, list.text("list_type").unwrap_or("popular"))?;
    let bearer = format!("Bearer {token}");
    let sort = if fixed.is_empty() {
        ""
    } else {
        "&sort_by=popularity.desc"
    };
    let mut items: Vec<ListItem> = Vec::new();
    for page in 1..=MAX_REQUESTS {
        let url = format!("{ORIGIN}{path}?{fixed}page={page}{sort}");
        let document = get_json(transport, url, &[("authorization", &bearer)]).await?;
        let (parsed, total_pages, raw) = parse_page(&document)?;
        items.extend(parsed);
        let last = total_pages.is_some_and(|total| page as i64 >= total);
        if items.len() >= limit || raw == 0 || last {
            items.truncate(limit);
            return finish(list.media, items);
        }
    }
    Err(FetchError::ResourceLimit)
}
