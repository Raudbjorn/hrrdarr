//! Simkl (shows and movies): the account's items in one list status.
//!
//! `GET https://api.simkl.com/sync/all-items/{shows|movies}/{status}` with the app's client id in the
//! `simkl-api-key` header and the user's OAuth token as `Authorization: Bearer`. Simkl answers an
//! empty list with `null`, `{}` or an empty body, all of which are a successful empty list. TV uses
//! `ids.tvdb`, movies `ids.tmdb`; both keep `ids.imdb`.
use super::*;
use crate::import_lists::model::ListItem;

const ORIGIN: &str = "https://api.simkl.com";
const STATUSES: &[&str] = &["watching", "plantowatch", "completed", "hold", "dropped"];

pub fn implementation() -> Implementation {
    Implementation {
        name: "simkl",
        label: "Simkl",
        description: "A Simkl account's shows or movies in one list status, read with the app client id and the account's access token.",
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
            "list_status",
            "List status",
            FieldKind::Select,
            "Which Simkl list to import.",
        )
        .options(STATUSES)
        .with_default(text_default("plantowatch")),
        field(
            "client_id",
            "Client ID",
            FieldKind::Text,
            "The Simkl app client id; sent as simkl-api-key.",
        )
        .required()
        .secret(),
        field(
            "access_token",
            "Access token",
            FieldKind::Text,
            "The account's OAuth access token; sent as a Bearer token.",
        )
        .required()
        .secret(),
    ]
}

pub fn parse(media: MediaDomain, document: &Value) -> Fetched {
    let (key, inner) = match media {
        MediaDomain::Tv => ("shows", "show"),
        MediaDomain::Movies => ("movies", "movie"),
    };
    let rows = match document {
        Value::Null => return Ok(Vec::new()),
        Value::Object(object) => match object.get(key) {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Array(rows)) => rows,
            Some(_) => return Err(FetchError::InvalidResponse),
        },
        _ => return Err(FetchError::InvalidResponse),
    };
    let mut items = Vec::with_capacity(rows.len().min(1024));
    for row in rows {
        let media_object = row
            .get(inner)
            .and_then(Value::as_object)
            .ok_or(FetchError::InvalidResponse)?;
        let ids = match media_object.get("ids") {
            None | Some(Value::Null) => None,
            Some(Value::Object(ids)) => Some(ids),
            Some(_) => return Err(FetchError::InvalidResponse),
        };
        let id = |name: &str| ids.and_then(|ids| ids.get(name));
        items.push(ListItem {
            tvdb_id: if media == MediaDomain::Tv {
                id_of(id("tvdb"))?
            } else {
                None
            },
            tmdb_id: if media == MediaDomain::Movies {
                id_of(id("tmdb"))?
            } else {
                None
            },
            imdb_id: imdb_of(id("imdb"))?,
            title: title_of(media_object.get("title")),
            year: year_of(media_object.get("year"))?,
        });
    }
    finish(media, items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let client_id = list.need_secret("client_id")?;
    let token = list.need_secret("access_token")?;
    let status = list.text("list_status").unwrap_or("plantowatch");
    if !STATUSES.contains(&status) {
        return Err(FetchError::InvalidSettings);
    }
    let kind = match list.media {
        MediaDomain::Tv => "shows",
        MediaDomain::Movies => "movies",
    };
    let bearer = format!("Bearer {token}");
    let out = Outgoing::get(format!("{ORIGIN}/sync/all-items/{kind}/{status}"))
        .header("accept", "application/json")
        .header("simkl-api-key", client_id)
        .header("authorization", bearer);
    let body = transport.send_limited(&out, MAX_BODY_BYTES).await?.body;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let document: Value = serde_json::from_slice(&body).map_err(|_| FetchError::InvalidResponse)?;
    parse(list.media, &document)
}
