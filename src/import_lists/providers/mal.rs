//! MyAnimeList (TV only): a user's anime list through the public v2 API.
//!
//! `GET https://api.myanimelist.net/v2/users/{username}/animelist` with the client id in the
//! `X-MAL-CLIENT-ID` header, 100 entries per page, following `paging.next` by advancing `offset`
//! (the URL MAL returns is never fetched). Items carry title and start year only: MAL ids are not
//! mapped to TVDB ids, so the sync resolves them by exact title.
use super::*;
use crate::import_lists::model::ListItem;

const ORIGIN: &str = "https://api.myanimelist.net";
const PAGE_SIZE: usize = 100;
const STATUSES: &[&str] = &[
    "all",
    "watching",
    "completed",
    "on_hold",
    "dropped",
    "plan_to_watch",
];

pub fn implementation() -> Implementation {
    Implementation {
        name: "mal",
        label: "MyAnimeList",
        description: "A MyAnimeList user's anime list filtered by status. Entries carry title and year only; MAL ids are not mapped to TVDB ids, so items are resolved by exact title.",
        domains: &[MediaDomain::Tv],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "username",
            "Username",
            FieldKind::Text,
            "The MyAnimeList user name (the list must be public).",
        )
        .required()
        .max_length(64),
        field(
            "status",
            "Status",
            FieldKind::Select,
            "Which list status to import; all imports every status.",
        )
        .options(STATUSES)
        .with_default(text_default("watching")),
        field(
            "client_id",
            "Client ID",
            FieldKind::Text,
            "A MyAnimeList API client id; sent as X-MAL-CLIENT-ID.",
        )
        .required()
        .secret(),
    ]
}

/// The request URL for one page. The user name is percent-encoded as a single path segment.
pub fn page_url(username: &str, status: Option<&str>, offset: usize) -> Result<String, FetchError> {
    if username.is_empty() || username == "." || username == ".." {
        return Err(FetchError::InvalidSettings);
    }
    let mut url = url::Url::parse(ORIGIN).map_err(|_| FetchError::InvalidSettings)?;
    url.path_segments_mut()
        .map_err(|_| FetchError::InvalidSettings)?
        .extend(["v2", "users", username, "animelist"]);
    let mut query = format!("limit={PAGE_SIZE}&offset={offset}&fields=start_season");
    if let Some(status) = status {
        query.push_str("&status=");
        query.push_str(&encode(status));
    }
    url.set_query(Some(&query));
    Ok(url.into())
}

/// Items of one page and whether MAL announced a further page.
pub fn parse_page(document: &Value) -> Result<(Vec<ListItem>, usize, bool), FetchError> {
    let object = document.as_object().ok_or(FetchError::InvalidResponse)?;
    let data = object
        .get("data")
        .and_then(Value::as_array)
        .ok_or(FetchError::InvalidResponse)?;
    let next = match object.get("paging") {
        None | Some(Value::Null) => false,
        Some(Value::Object(paging)) => match paging.get("next") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.is_empty(),
            Some(_) => return Err(FetchError::InvalidResponse),
        },
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let mut items = Vec::with_capacity(data.len());
    for entry in data {
        let node = entry
            .get("node")
            .and_then(Value::as_object)
            .ok_or(FetchError::InvalidResponse)?;
        id_of(node.get("id"))?;
        let year = match node.get("start_season") {
            None | Some(Value::Null) => None,
            Some(Value::Object(season)) => year_of(season.get("year"))?,
            Some(_) => return Err(FetchError::InvalidResponse),
        };
        items.push(ListItem {
            tvdb_id: None,
            tmdb_id: None,
            imdb_id: None,
            title: title_of(node.get("title")),
            year,
        });
    }
    Ok((items, data.len(), next))
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let username = list.need_text("username")?;
    let client_id = list.need_secret("client_id")?;
    let status = list.text("status").unwrap_or("watching");
    if !STATUSES.contains(&status) {
        return Err(FetchError::InvalidSettings);
    }
    let status = (status != "all").then_some(status);
    let mut all = Vec::new();
    let mut offset = 0usize;
    for _ in 0..MAX_REQUESTS {
        let url = page_url(username, status, offset)?;
        let document = get_json(transport, url, &[("x-mal-client-id", client_id)]).await?;
        let (items, rows, next) = parse_page(&document)?;
        all.extend(items);
        if all.len() > MAX_ITEMS * 2 {
            return Err(FetchError::ResourceLimit);
        }
        if !next {
            return finish(list.media, all);
        }
        // A "next" page after an empty one would loop without progress.
        if rows == 0 {
            return Err(FetchError::InvalidResponse);
        }
        offset += rows;
    }
    Err(FetchError::ResourceLimit)
}
