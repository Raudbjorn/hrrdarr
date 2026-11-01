//! Trakt lists (Sonarr "Trakt List/Popular/User", Radarr equivalents) over the public Trakt v2 API.
//!
//! Authentication is a Trakt application client id (`trakt-api-key`) plus an optional static OAuth
//! access token (`Authorization: Bearer`) that private lists and watchlists need. The OAuth device
//! flow and token refresh are NOT implemented: the user supplies an access token obtained elsewhere
//! and it is used until it expires.
//!
//! Identity: shows carry the TVDB id, movies the TMDb id, and both always carry the IMDb id. Pages
//! are requested with `page`/`limit`; fetching stops at `limit` items or at the first short page, and
//! a list needing more than [`MAX_REQUESTS`] requests fails with `resource_limit`.
use super::*;
use crate::import_lists::model::{ListItem, SettingValue};

const ORIGIN: &str = "https://api.trakt.tv";
const API_VERSION: &str = "2";
const PAGE_SIZE_MAX: usize = 100;
const LIMIT_DEFAULT: i64 = 100;
const LIMIT_MAX: i64 = 1000;
const NAME_MAX_BYTES: usize = 128;

pub fn implementation() -> Implementation {
    Implementation {
        name: "trakt",
        label: "Trakt",
        description: "Popular, trending, anticipated and most-watched lists, a user's watchlist or a custom list from Trakt. \
            Uses a Trakt client id and an optional static OAuth access token for private lists; the OAuth device flow and \
            token refresh are not implemented, so the access token must be renewed by hand when it expires.",
        domains: &[MediaDomain::Tv, MediaDomain::Movies],
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
            "Which Trakt list to import.",
        )
        .options(&[
            "popular",
            "trending",
            "anticipated",
            "watched",
            "user_watchlist",
            "user_list",
        ])
        .with_default(text_default("popular")),
        field(
            "username",
            "Username",
            FieldKind::Text,
            "Trakt user slug; required for user_watchlist and user_list.",
        )
        .max_length(NAME_MAX_BYTES as u32),
        field(
            "list_slug",
            "List slug",
            FieldKind::Text,
            "Slug of the custom list; required for user_list.",
        )
        .max_length(NAME_MAX_BYTES as u32),
        field(
            "limit",
            "Item limit",
            FieldKind::Number,
            "Total items to import (page size is at most 100).",
        )
        .range(1, LIMIT_MAX)
        .with_default(SettingValue::Int(LIMIT_DEFAULT)),
        field(
            "client_id",
            "Client ID",
            FieldKind::Text,
            "Trakt application client id; sent as trakt-api-key.",
        )
        .required()
        .secret(),
        field(
            "access_token",
            "Access token",
            FieldKind::Text,
            "Optional static OAuth access token for private lists and watchlists.",
        )
        .secret(),
    ]
}

/// Percent-encodes one path segment (a space is `%20`, a literal `+` is `%2B`).
fn segment(value: &str) -> String {
    encode(value).replace('+', "%20")
}

fn name_of(list: &ListRuntime, key: &str) -> Result<String, FetchError> {
    let value = list.need_text(key)?;
    if value.len() > NAME_MAX_BYTES || value.chars().any(char::is_control) {
        return Err(FetchError::InvalidSettings);
    }
    Ok(segment(value))
}

fn endpoint(list: &ListRuntime, list_type: &str, kind: &str) -> Result<String, FetchError> {
    match list_type {
        "popular" | "trending" | "anticipated" | "watched" => Ok(format!("/{kind}/{list_type}")),
        "user_watchlist" => Ok(format!(
            "/users/{}/watchlist/{kind}",
            name_of(list, "username")?
        )),
        "user_list" => Ok(format!(
            "/users/{}/lists/{}/items/{kind}",
            name_of(list, "username")?,
            name_of(list, "list_slug")?
        )),
        _ => Err(FetchError::InvalidSettings),
    }
}

/// One entry of a response. `Ok(None)` is an entry of the other media kind.
fn parse_entry(
    media: MediaDomain,
    bare: bool,
    entry: &Value,
) -> Result<Option<ListItem>, FetchError> {
    let object = entry.as_object().ok_or(FetchError::InvalidResponse)?;
    let (singular, tv) = match media {
        MediaDomain::Tv => ("show", true),
        MediaDomain::Movies => ("movie", false),
    };
    let node = if bare {
        object
    } else {
        match object.get("type") {
            None | Some(Value::Null) => {}
            Some(Value::String(t)) if t == singular => {}
            Some(Value::String(_)) => return Ok(None),
            Some(_) => return Err(FetchError::InvalidResponse),
        }
        match object.get(singular) {
            Some(Value::Object(node)) => node,
            None | Some(Value::Null) => return Ok(None),
            Some(_) => return Err(FetchError::InvalidResponse),
        }
    };
    let ids = match node.get("ids") {
        None | Some(Value::Null) => None,
        Some(Value::Object(ids)) => Some(ids),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let id = |name: &str| ids.and_then(|ids| ids.get(name));
    Ok(Some(ListItem {
        tvdb_id: if tv { id_of(id("tvdb"))? } else { None },
        tmdb_id: if tv { None } else { id_of(id("tmdb"))? },
        imdb_id: imdb_of(id("imdb"))?,
        title: title_of(node.get("title")),
        year: year_of(node.get("year"))?,
    }))
}

/// Parses one page: the top level must be an array. Returns the page's raw entry count and items.
pub fn parse_page(
    media: MediaDomain,
    bare: bool,
    document: &Value,
) -> Result<(usize, Vec<ListItem>), FetchError> {
    let array = document.as_array().ok_or(FetchError::InvalidResponse)?;
    let mut items = Vec::new();
    for entry in array {
        items.extend(parse_entry(media, bare, entry)?);
    }
    Ok((array.len(), items))
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let client_id = list.need_secret("client_id")?;
    let list_type = list.text("list_type").unwrap_or("popular");
    let limit = list.int("limit").unwrap_or(LIMIT_DEFAULT);
    if !(1..=LIMIT_MAX).contains(&limit) {
        return Err(FetchError::InvalidSettings);
    }
    let limit = limit as usize;
    let kind = match list.media {
        MediaDomain::Tv => "shows",
        MediaDomain::Movies => "movies",
    };
    let path = endpoint(list, list_type, kind)?;
    let bare = list_type == "popular";
    let bearer = list.secret("access_token").map(|t| format!("Bearer {t}"));
    let mut headers: Vec<(&str, &str)> = vec![
        ("trakt-api-version", API_VERSION),
        ("trakt-api-key", client_id),
    ];
    if let Some(value) = &bearer {
        headers.push(("authorization", value));
    }
    let page_size = limit.min(PAGE_SIZE_MAX);
    let mut items: Vec<ListItem> = Vec::new();
    for page in 1..=MAX_REQUESTS {
        let url = format!("{ORIGIN}{path}?page={page}&limit={page_size}");
        let document = get_json(transport, url, &headers).await?;
        let (raw, parsed) = parse_page(list.media, bare, &document)?;
        items.extend(parsed);
        if items.len() >= limit || raw < page_size {
            items.truncate(limit);
            return finish(list.media, items);
        }
    }
    Err(FetchError::ResourceLimit)
}
