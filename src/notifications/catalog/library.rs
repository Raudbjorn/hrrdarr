//! Media-server notifiers: ask Kodi, Emby/Jellyfin or Plex to rescan after files change.
use super::{BOTH, Build, Config, Implementation, changes_library, clip};
use crate::{
    api::MediaDomain,
    notifications::{
        events::{EventKind, Message},
        model::{Field, ImplementationKind},
        transport::{Method, Outgoing, SendError, Transport, basic_auth},
    },
};
use serde_json::{Value, json};

fn join_path(base: &str, tail: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        tail.trim_start_matches('/')
    )
}

fn kodi(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let url = join_path(c.text("url"), "jsonrpc");
    let rpc = |method: &str, params: Value, id: u32| {
        let mut request = Outgoing::post_json(
            url.clone(),
            json!({"jsonrpc":"2.0","method":method,"params":params,"id":id}),
        );
        if let Some(user) = c.opt("username") {
            request = request.header("Authorization", basic_auth(user, c.text("password")));
        }
        request
    };
    let mut out = Vec::new();
    if c.bool("notify") && !matches!(m.kind, EventKind::Delete | EventKind::FileDelete) {
        out.push(rpc(
            "GUI.ShowNotification",
            json!({"title":clip(&m.title,120),"message":clip(&m.text,500),
                   "displaytime":c.int("display_time").unwrap_or(5) * 1000}),
            1,
        ));
    }
    if changes_library(m.kind) {
        if c.bool("update_library")
            && matches!(
                m.kind,
                EventKind::Import | EventKind::Rename | EventKind::Add | EventKind::Test
            )
        {
            out.push(rpc("VideoLibrary.Scan", json!({}), 2));
        }
        if c.bool("clean_library")
            && matches!(
                m.kind,
                EventKind::Delete | EventKind::FileDelete | EventKind::Rename
            )
        {
            out.push(rpc("VideoLibrary.Clean", json!({}), 3));
        }
    }
    Ok(out)
}

fn media_browser(c: &Config, m: &Message) -> Result<Vec<Outgoing>, &'static str> {
    let token = c.text("api_key").to_owned();
    let authed = |request: Outgoing| request.header("X-Emby-Token", token.clone());
    let mut out = Vec::new();
    if c.bool("notify") && !matches!(m.kind, EventKind::Delete | EventKind::FileDelete) {
        out.push(authed(Outgoing::post_json(
            join_path(c.text("url"), "Notifications/Admin"),
            json!({"Name":clip(&m.title,120),"Description":clip(&m.text,500),"Level":"Normal"}),
        )));
    }
    if c.bool("update_library") && changes_library(m.kind) && m.kind != EventKind::Test {
        if m.paths.is_empty() {
            out.push(authed(Outgoing {
                method: Method::Post,
                url: join_path(c.text("url"), "Library/Refresh"),
                headers: Vec::new(),
                body: crate::notifications::transport::Body::None,
            }));
        } else {
            let update = match m.kind {
                EventKind::Delete | EventKind::FileDelete => "Deleted",
                EventKind::Rename => "Modified",
                _ => "Created",
            };
            let updates: Vec<Value> = m
                .paths
                .iter()
                .map(|p| json!({"Path":p,"UpdateType":update}))
                .collect();
            out.push(authed(Outgoing::post_json(
                join_path(c.text("url"), "Library/Media/Updated"),
                json!({"Updates":updates}),
            )));
        }
    }
    Ok(out)
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/')
        .map_or(path, |(dir, _)| if dir.is_empty() { "/" } else { dir })
}

/// Plex flow: list library sections, then refresh each section whose location contains an event
/// path (or every section of the right type when the event has no path). Returns the number of
/// refresh requests issued.
pub async fn run_plex(
    transport: &Transport,
    c: &Config<'_>,
    m: &Message,
) -> Result<usize, SendError> {
    if !c.bool("update_library") || !changes_library(m.kind) {
        return Ok(0);
    }
    let base = c.text("url").to_owned();
    let token = c.text("auth_token").to_owned();
    let listing = transport
        .send_limited(
            &Outgoing::get(join_path(&base, "library/sections"))
                .header("X-Plex-Token", token.clone())
                .header("Accept", "application/json"),
            1024 * 1024,
        )
        .await?;
    let document: Value = serde_json::from_slice(&listing.body).map_err(|_| SendError::Rejected)?;
    let want = match m.domain {
        MediaDomain::Tv => "show",
        MediaDomain::Movies => "movie",
    };
    let sections = document["MediaContainer"]["Directory"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut requests = Vec::new();
    for section in sections.iter().take(64) {
        if section["type"].as_str() != Some(want) {
            continue;
        }
        let Some(key) = section["key"]
            .as_str()
            .filter(|k| k.bytes().all(|b| b.is_ascii_digit()) && !k.is_empty())
        else {
            continue;
        };
        let locations: Vec<&str> = section["Location"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l["path"].as_str()).collect())
            .unwrap_or_default();
        let refresh = join_path(&base, &format!("library/sections/{key}/refresh"));
        if m.paths.is_empty() {
            requests.push(refresh);
            continue;
        }
        for path in &m.paths {
            let inside = locations.iter().any(|root| {
                let root = root.trim_end_matches('/');
                path.strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/'))
            });
            if inside {
                let mut url = url::Url::parse(&refresh).map_err(|_| SendError::InvalidRequest)?;
                let dir = if m.kind == EventKind::Import {
                    parent(path)
                } else {
                    path.as_str()
                };
                url.query_pairs_mut().append_pair("path", dir);
                requests.push(url.into());
            }
        }
    }
    requests.sort();
    requests.dedup();
    requests.truncate(32);
    for url in &requests {
        transport
            .send(&Outgoing::get(url.clone()).header("X-Plex-Token", token.clone()))
            .await?;
    }
    Ok(requests.len())
}

pub fn implementations() -> Vec<Implementation> {
    let library = ImplementationKind::LibraryUpdate;
    vec![
        Implementation {
            name: "kodi",
            label: "Kodi (XBMC)",
            description: "Show a GUI notification and ask Kodi to scan or clean its video library over JSON-RPC.",
            kind: library,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("url", "Kodi URL")
                        .required()
                        .help("For example http://kodi.local:8080"),
                    Field::text("username", "Username"),
                    Field::secret("password", "Password"),
                    Field::number("display_time", "Notification seconds", 1, 60).default_int(5),
                    Field::boolean("notify", "Show GUI notification", true),
                    Field::boolean("update_library", "Update library", true),
                    Field::boolean("clean_library", "Clean library on delete", false),
                ]
            },
            build: Build::Http(kodi),
        },
        Implementation {
            name: "media_browser",
            label: "Emby / Jellyfin",
            description: "Report changed paths to an Emby or Jellyfin server (library update and optional admin notification).",
            kind: library,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("url", "Server URL").required(),
                    Field::secret("api_key", "API key").required(),
                    Field::boolean("update_library", "Update library", true),
                    Field::boolean("notify", "Send admin notification", false),
                ]
            },
            build: Build::Http(media_browser),
        },
        Implementation {
            name: "plex",
            label: "Plex Media Server",
            description: "Refresh the Plex library sections that contain changed files.",
            kind: library,
            domains: BOTH,
            fields: || {
                vec![
                    Field::url("url", "Server URL").required(),
                    Field::secret("auth_token", "Auth token").required(),
                    Field::boolean("update_library", "Update library", true),
                ]
            },
            build: Build::Plex,
        },
    ]
}
