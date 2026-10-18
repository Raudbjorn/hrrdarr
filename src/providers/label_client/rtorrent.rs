//! rTorrent XML-RPC wire (`POST <endpoint>`, usually an nginx or ruTorrent `/RPC2` mount in front
//! of rTorrent's SCGI socket), optional HTTP basic authentication. Torrents are labelled with the
//! ruTorrent-compatible `d.custom1` and identified by info hash.
use super::{
    AddReply, AddRequest, MAX_FILES, MAX_LISTED_TORRENTS, RawFile, RawState, RawTorrent, Result,
    Wire, WireInfo, valid_hash,
};
use crate::providers::{
    Credentials,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{AddSource, Control, QbitError},
};
use std::time::Duration;

const MIN_VERSION: (u32, u32) = (0, 9);
const MAX_XML_DEPTH: usize = 12;
const MAX_XML_NODES: u32 = 400_000;
/// rTorrent loads asynchronously: `load.*` returns before the torrent is listed.
const VISIBLE_ATTEMPTS: u32 = 15;
const VISIBLE_INTERVAL: Duration = Duration::from_millis(200);
const PRIORITY_NORMAL: i64 = 2;
const PRIORITY_HIGH: i64 = 3;
const LIST_FIELDS: [&str; 15] = [
    "d.hash=",
    "d.name=",
    "d.custom1=",
    "d.directory=",
    "d.is_multi_file=",
    "d.size_bytes=",
    "d.completed_bytes=",
    "d.state=",
    "d.is_active=",
    "d.complete=",
    "d.hashing=",
    "d.message=",
    "d.down.rate=",
    "d.up.rate=",
    "d.ratio=",
];

enum Arg {
    Str(String),
    Int(i64),
    Bytes(Vec<u8>),
}
impl Arg {
    fn s(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}
#[derive(Debug)]
enum Xv {
    Int(i64),
    Str(String),
    Arr(Vec<Xv>),
    Other,
}
impl Xv {
    fn int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
    fn arr(&self) -> Option<&[Xv]> {
        match self {
            Self::Arr(a) => Some(a),
            _ => None,
        }
    }
}

fn escape(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // XML 1.0 cannot carry other control characters at all.
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}
fn request_xml(method: &str, args: &[Arg]) -> Vec<u8> {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    escape(method, &mut out);
    out.push_str("</methodName><params>");
    for arg in args {
        out.push_str("<param><value>");
        match arg {
            Arg::Str(s) => {
                out.push_str("<string>");
                escape(s, &mut out);
                out.push_str("</string>");
            }
            Arg::Int(n) => out.push_str(&format!("<i8>{n}</i8>")),
            Arg::Bytes(b) => out.push_str(&format!(
                "<base64>{}</base64>",
                crate::providers::transmission::base64(b)
            )),
        }
        out.push_str("</value></param>");
    }
    out.push_str("</params></methodCall>");
    out.into_bytes()
}
fn element_children<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(roxmltree::Node::is_element)
}
fn parse_value(node: roxmltree::Node, depth: usize) -> Result<Xv> {
    if depth > MAX_XML_DEPTH {
        return Err(QbitError::InvalidResponse);
    }
    let Some(inner) = element_children(node).next() else {
        // A bare value is a string.
        return Ok(Xv::Str(node.text().unwrap_or("").to_owned()));
    };
    let text = inner.text().unwrap_or("").trim();
    Ok(match inner.tag_name().name() {
        "i4" | "i8" | "int" | "boolean" => {
            Xv::Int(text.parse().map_err(|_| QbitError::InvalidResponse)?)
        }
        "string" => Xv::Str(inner.text().unwrap_or("").to_owned()),
        "array" => {
            let data = element_children(inner)
                .find(|n| n.tag_name().name() == "data")
                .ok_or(QbitError::InvalidResponse)?;
            Xv::Arr(
                element_children(data)
                    .filter(|n| n.tag_name().name() == "value")
                    .map(|n| parse_value(n, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        _ => Xv::Other,
    })
}
/// Parses a `methodResponse`; a `fault` is a rejection whose text is never reflected.
fn parse_response(body: &[u8]) -> Result<Xv> {
    let text = std::str::from_utf8(body).map_err(|_| QbitError::InvalidResponse)?;
    // DTDs (entity expansion) are refused by roxmltree's defaults; the node count is bounded.
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            ..Default::default()
        },
    )
    .map_err(|_| QbitError::InvalidResponse)?;
    let root = document.root_element();
    if root.tag_name().name() != "methodResponse" {
        return Err(QbitError::InvalidResponse);
    }
    let first = element_children(root)
        .next()
        .ok_or(QbitError::InvalidResponse)?;
    match first.tag_name().name() {
        "fault" => Err(QbitError::Rejected),
        "params" => {
            let value = element_children(first)
                .find(|n| n.tag_name().name() == "param")
                .and_then(|p| element_children(p).find(|n| n.tag_name().name() == "value"))
                .ok_or(QbitError::InvalidResponse)?;
            parse_value(value, 0)
        }
        _ => Err(QbitError::InvalidResponse),
    }
}
/// ruTorrent stores labels URL-encoded in `d.custom1`.
fn decode_label(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Some(byte) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) struct RtorrentWire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    authorization: Option<String>,
    secrets: Vec<String>,
}
impl<'o, 'a> RtorrentWire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        let (authorization, secrets) = match credentials {
            None => (None, Vec::new()),
            Some(Credentials::UsernamePassword { username, password }) => {
                // RFC 7617: the user-id cannot contain a colon.
                if username.contains(':') {
                    return Err(QbitError::InvalidRequest);
                }
                (
                    Some(format!(
                        "Basic {}",
                        crate::providers::transmission::base64(
                            format!("{username}:{password}").as_bytes()
                        )
                    )),
                    [username, password]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .collect(),
                )
            }
            Some(_) => return Err(QbitError::InvalidRequest),
        };
        url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        Ok(Self {
            operation,
            url: endpoint.to_owned(),
            authorization,
            secrets,
        })
    }

    async fn call(&mut self, method: &str, args: &[Arg]) -> Result<Xv> {
        let mut headers = Vec::new();
        if let Some(value) = &self.authorization {
            headers.push(("Authorization".to_owned(), value.clone()));
        }
        let response = self
            .operation
            .request(
                &self.url,
                &[],
                &headers,
                HttpRequestBody::Xml(request_xml(method, args)),
            )
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(if response.status == 404 || response.status >= 500 {
                QbitError::Http(HttpError::Transport)
            } else {
                QbitError::InvalidResponse
            });
        }
        parse_response(&response.body)
    }

    async fn all(&mut self) -> Result<Vec<RawTorrent>> {
        let mut args = vec![Arg::s(""), Arg::s("main")];
        args.extend(LIST_FIELDS.iter().map(|f| Arg::s(f)));
        let value = self.call("d.multicall2", &args).await?;
        let rows = value.arr().ok_or(QbitError::InvalidResponse)?;
        if rows.len() > MAX_LISTED_TORRENTS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        rows.iter().map(torrent).collect()
    }
}

fn torrent(row: &Xv) -> Result<RawTorrent> {
    let cells = row.arr().ok_or(QbitError::InvalidResponse)?;
    if cells.len() != LIST_FIELDS.len() {
        return Err(QbitError::InvalidResponse);
    }
    let text = |i: usize, limit: usize| -> Result<String> {
        let s = cells[i].text().ok_or(QbitError::InvalidResponse)?;
        if s.len() > limit || s.chars().any(char::is_control) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(s.to_owned())
    };
    let int = |i: usize| -> Result<i64> { cells[i].int().ok_or(QbitError::InvalidResponse) };
    let hash = text(0, 64)?.to_ascii_lowercase();
    if !valid_hash(&hash) {
        return Err(QbitError::InvalidResponse);
    }
    let name = text(1, 1024)?;
    let directory = text(3, 4096)?;
    let multi = int(4)? != 0;
    let size = int(5)?.max(0) as u64;
    let complete = int(9)? != 0;
    let downloaded = if complete {
        size
    } else {
        (int(6)?.max(0) as u64).min(size)
    };
    let message = text(11, 4096)?;
    // A multi-file torrent's directory already includes its own name; the engine's file paths are
    // relative to the folder that contains it.
    let save_path = if multi {
        match directory.rsplit_once('/') {
            Some((parent, last)) if last == name && !parent.is_empty() => parent.to_owned(),
            _ => directory,
        }
    } else {
        directory
    };
    let state = if int(10)? != 0 {
        RawState::Checking
    } else if size == 0 {
        RawState::Metadata
    } else if !message.is_empty() && !message.starts_with("Tracker:") {
        RawState::Error
    } else if int(7)? == 0 {
        RawState::Paused
    } else if complete {
        RawState::Seeding
    } else if int(8)? != 0 {
        RawState::Downloading
    } else {
        RawState::Queued
    };
    Ok(RawTorrent {
        hash,
        name,
        label: decode_label(&text(2, 1024)?),
        save_path,
        size_bytes: size,
        downloaded_bytes: downloaded,
        state,
        download_rate: int(12)?.max(0) as u64,
        upload_rate: int(13)?.max(0) as u64,
        eta_seconds: None,
        // rTorrent reports the ratio in thousandths.
        ratio: int(14)?.max(0) as f64 / 1000.0,
        seeding_seconds: None,
    })
}

impl Wire for RtorrentWire<'_, '_> {
    fn redact(&self, mut value: String) -> String {
        for secret in &self.secrets {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        value
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        let value = self.call("system.client_version", &[]).await?;
        let raw = value.text().ok_or(QbitError::InvalidResponse)?.trim();
        if raw.is_empty() || raw.len() > 64 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(QbitError::InvalidResponse);
        }
        let mut parts = raw.split('.').map(|p| p.parse::<u32>());
        match (
            parts.next().and_then(|p| p.ok()),
            parts.next().and_then(|p| p.ok()),
        ) {
            (Some(major), Some(minor)) if (major, minor) >= MIN_VERSION => {}
            (Some(_), Some(_)) => return Err(QbitError::UnsupportedVersion),
            _ => return Err(QbitError::InvalidResponse),
        }
        let default_dir = match self.call("directory.default", &[]).await {
            Ok(v) => v.text().unwrap_or("").to_owned(),
            Err(QbitError::Rejected) => String::new(),
            Err(error) => return Err(error),
        };
        Ok(WireInfo {
            version: raw.to_owned(),
            default_dir,
            // d.custom1 labels need no declaration or plugin.
            labels_supported: true,
        })
    }
    async fn list(&mut self, _label: Option<&str>) -> Result<Vec<RawTorrent>> {
        self.all().await
    }
    async fn get(&mut self, hash: &str) -> Result<Option<RawTorrent>> {
        Ok(self.all().await?.into_iter().find(|t| t.hash == hash))
    }
    async fn files(&mut self, hash: &str) -> Result<Vec<RawFile>> {
        let upper = hash.to_ascii_uppercase();
        let name = self.call("d.name", &[Arg::Str(upper.clone())]).await?;
        let name = name.text().ok_or(QbitError::InvalidResponse)?.to_owned();
        let multi = self
            .call("d.is_multi_file", &[Arg::Str(upper.clone())])
            .await?
            .int()
            .ok_or(QbitError::InvalidResponse)?
            != 0;
        let mut args = vec![Arg::Str(upper), Arg::s("")];
        args.extend(
            [
                "f.path=",
                "f.size_bytes=",
                "f.completed_chunks=",
                "f.size_chunks=",
                "f.priority=",
            ]
            .iter()
            .map(|f| Arg::s(f)),
        );
        let value = self.call("f.multicall", &args).await?;
        let rows = value.arr().ok_or(QbitError::InvalidResponse)?;
        if rows.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        rows.iter()
            .map(|row| {
                let cells = row.arr().ok_or(QbitError::InvalidResponse)?;
                if cells.len() != 5 {
                    return Err(QbitError::InvalidResponse);
                }
                let path = cells[0].text().ok_or(QbitError::InvalidResponse)?;
                let size = cells[1].int().ok_or(QbitError::InvalidResponse)?.max(0) as u64;
                let done = cells[2].int().ok_or(QbitError::InvalidResponse)?.max(0) as f64;
                let chunks = cells[3].int().ok_or(QbitError::InvalidResponse)?.max(0) as f64;
                Ok(RawFile {
                    path: if multi {
                        format!("{name}/{path}")
                    } else {
                        path.to_owned()
                    },
                    size_bytes: size,
                    downloaded_bytes: if chunks == 0.0 {
                        0
                    } else {
                        (size as f64 * (done / chunks).min(1.0)).round() as u64
                    },
                    wanted: cells[4].int().ok_or(QbitError::InvalidResponse)? > 0,
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        // The label is a command run by rTorrent at load time (before start), so the torrent
        // never exists unlabelled. Labels are restricted to a command-safe alphabet.
        if request.label.is_empty()
            || !request.label.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
        {
            return Err(QbitError::InvalidRequest);
        }
        let mut args = vec![Arg::s("")];
        let method = match request.source {
            AddSource::Magnet(uri) => {
                args.push(Arg::Str(uri.clone()));
                if request.paused {
                    "load.normal"
                } else {
                    "load.start"
                }
            }
            AddSource::Torrent { bytes, .. } => {
                args.push(Arg::Bytes(bytes.clone()));
                if request.paused {
                    "load.raw"
                } else {
                    "load.raw_start"
                }
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        };
        args.push(Arg::Str(format!("d.custom1.set={}", request.label)));
        if request.first {
            args.push(Arg::Str(format!("d.priority.set={PRIORITY_HIGH}")));
        }
        self.call(method, &args).await?;
        for _ in 0..VISIBLE_ATTEMPTS {
            if self.get(request.hash).await?.is_some() {
                return Ok(AddReply::Added);
            }
            tokio::time::sleep(VISIBLE_INTERVAL).await;
        }
        // Not listed yet: the engine's verification decides (and reconciliation follows).
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, hash: &str, label: &str, first: bool) -> Result<()> {
        let upper = hash.to_ascii_uppercase();
        self.call("d.custom1.set", &[Arg::Str(upper.clone()), Arg::s(label)])
            .await?;
        if first {
            self.call(
                "d.priority.set",
                &[Arg::Str(upper), Arg::Int(PRIORITY_HIGH)],
            )
            .await?;
        }
        Ok(())
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        let upper = Arg::Str(hash.to_ascii_uppercase());
        match action {
            Control::Pause => self.call("d.stop", &[upper]).await,
            Control::Resume => self.call("d.start", &[upper]).await,
            // rTorrent cannot delete data through XML-RPC; erase only forgets the torrent.
            Control::Remove {
                delete_files: false,
            } => self.call("d.erase", &[upper]).await,
            Control::Priority { first } => {
                self.call(
                    "d.priority.set",
                    &[
                        upper,
                        Arg::Int(if *first {
                            PRIORITY_HIGH
                        } else {
                            PRIORITY_NORMAL
                        }),
                    ],
                )
                .await
            }
            _ => return Err(QbitError::UnsupportedFeature),
        }
        .map(|_| ())
    }
}
