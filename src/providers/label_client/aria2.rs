//! Aria2 JSON-RPC wire (`POST <endpoint>`, usually `/jsonrpc`), optional RPC secret sent as the
//! leading `token:<secret>` parameter. aria2 has no labels: a domain's downloads live in the
//! subfolder `<default dir>/<label>` of aria2's download directory, and a download belongs to the
//! domain whose subfolder is its `dir`. BitTorrent downloads are identified by their info hash;
//! aria2's own GIDs stay inside this wire.
use super::{
    AddReply, AddRequest, MAX_FILES, MAX_LISTED_TORRENTS, RawFile, RawState, RawTorrent, Result,
    Wire, WireInfo, valid_hash,
};
use crate::providers::{
    Credentials,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{AddSource, Control, QbitError},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MIN_VERSION: (u32, u32) = (1, 18);
/// Maximum entries requested in each waiting/stopped page.
const WINDOW: usize = 1000;
const STATUS_KEYS: [&str; 13] = [
    "gid",
    "status",
    "totalLength",
    "completedLength",
    "uploadLength",
    "downloadSpeed",
    "uploadSpeed",
    "infoHash",
    "dir",
    "bittorrent",
    "followedBy",
    "errorCode",
    "seeder",
];

pub(crate) struct Aria2Wire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    secret: Option<String>,
    request_id: u64,
    default_dir: String,
    /// info hash -> GID of the last listing, for controls.
    gids: BTreeMap<String, String>,
}
impl<'o, 'a> Aria2Wire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        let secret = match credentials {
            None => None,
            Some(Credentials::ApiKey { api_key }) => Some(api_key.clone()),
            Some(_) => return Err(QbitError::InvalidRequest),
        };
        url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        Ok(Self {
            operation,
            url: endpoint.to_owned(),
            secret,
            request_id: 0,
            default_dir: String::new(),
            gids: BTreeMap::new(),
        })
    }

    async fn call(&mut self, method: &str, params: Vec<Value>) -> Result<Value> {
        self.request_id = self
            .request_id
            .checked_add(1)
            .ok_or(QbitError::InvalidRequest)?;
        let mut all = Vec::with_capacity(params.len() + 1);
        if let Some(secret) = &self.secret {
            all.push(json!(format!("token:{secret}")));
        }
        all.extend(params);
        let body = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","id":self.request_id,"method":method,"params":all}),
        )
        .map_err(|_| QbitError::InvalidRequest)?;
        let response = self
            .operation
            .request(&self.url, &[], &[], HttpRequestBody::Json(body))
            .await?;
        // aria2 answers a bad secret with HTTP 401 and a JSON-RPC error body.
        if response.status == 401 || response.status == 403 {
            return Err(HttpError::Authentication.into());
        }
        if !(200..300).contains(&response.status) && response.status != 400 {
            return Err(if response.status == 404 || response.status >= 500 {
                QbitError::Http(HttpError::Transport)
            } else {
                QbitError::InvalidResponse
            });
        }
        let mut value: Value =
            serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)?;
        if value.get("id").and_then(Value::as_u64) != Some(self.request_id) {
            return Err(QbitError::InvalidResponse);
        }
        if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
            // A wrong RPC secret is answered as "Unauthorized" (code 1, HTTP 400).
            if error.get("message").and_then(Value::as_str) == Some("Unauthorized") {
                return Err(HttpError::Authentication.into());
            }
            return Err(QbitError::Rejected);
        }
        value
            .get_mut("result")
            .map(Value::take)
            .ok_or(QbitError::InvalidResponse)
    }

    fn folder(&self, label: &str) -> String {
        format!("{}/{label}", self.default_dir)
    }
    async fn all(&mut self) -> Result<Vec<RawTorrent>> {
        let mut out = Vec::new();
        let mut gids = BTreeMap::new();
        let mut seen = 0;
        for method in ["aria2.tellActive", "aria2.tellWaiting", "aria2.tellStopped"] {
            let mut offset = 0;
            loop {
                let params = if method == "aria2.tellActive" {
                    vec![json!(STATUS_KEYS)]
                } else {
                    vec![json!(offset), json!(WINDOW), json!(STATUS_KEYS)]
                };
                let page = self.call(method, params).await?;
                let entries = page.as_array().ok_or(QbitError::InvalidResponse)?;
                // Count even skipped/duplicate rows so pagination is bounded for shared clients.
                seen += entries.len();
                if seen > MAX_LISTED_TORRENTS {
                    return Err(HttpError::ResponseTooLarge.into());
                }
                for entry in entries {
                    if let Some((gid, torrent)) = torrent(entry, &self.default_dir)? {
                        // Keep the first, most active, entry when a hash appears in several lists.
                        if gids.contains_key(&torrent.hash) {
                            continue;
                        }
                        gids.insert(torrent.hash.clone(), gid);
                        out.push(torrent);
                    }
                }
                if method == "aria2.tellActive" || entries.len() < WINDOW {
                    break;
                }
                offset += entries.len();
            }
        }
        self.gids = gids;
        Ok(out)
    }
    async fn gid(&mut self, hash: &str) -> Result<String> {
        if !self.gids.contains_key(hash) {
            self.all().await?;
        }
        self.gids.get(hash).cloned().ok_or(QbitError::NotFound)
    }
}

fn number(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_u64()))
        .unwrap_or(0)
}
fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_owned()
}
/// One tellStatus row; downloads that are not BitTorrent, and metadata stubs that were replaced by
/// the real download, are skipped.
fn torrent(entry: &Value, default_dir: &str) -> Result<Option<(String, RawTorrent)>> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
    let hash = text("infoHash").to_ascii_lowercase();
    if hash.is_empty() {
        return Ok(None);
    }
    if !valid_hash(&hash) {
        return Err(QbitError::InvalidResponse);
    }
    if entry
        .get("followedBy")
        .and_then(Value::as_array)
        .is_some_and(|f| !f.is_empty())
    {
        return Ok(None);
    }
    let gid = text("gid").to_owned();
    if gid.is_empty() || gid.len() > 64 {
        return Err(QbitError::InvalidResponse);
    }
    let dir = normalize(text("dir"));
    let base = normalize(default_dir);
    // The owning label is the last component of a directory directly below aria2's default one.
    let label = match dir.rsplit_once('/') {
        Some((parent, last)) if !base.is_empty() && parent == base => last.to_owned(),
        _ => String::new(),
    };
    let name = entry
        .get("bittorrent")
        .and_then(|b| b.get("info"))
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if name.len() > 1024 || name.chars().any(char::is_control) {
        return Err(QbitError::InvalidResponse);
    }
    let size = number(entry, "totalLength");
    let done = number(entry, "completedLength");
    let download = number(entry, "downloadSpeed");
    let status = text("status");
    let state = if name.is_empty() && size == 0 {
        RawState::Metadata
    } else {
        match status {
            "active" => RawState::Downloading,
            "waiting" => RawState::Queued,
            "paused" => RawState::Paused,
            "error" => RawState::Error,
            // Finished downloads (seeding ended) keep their data.
            "complete" => RawState::Seeding,
            _ => RawState::Unknown,
        }
    };
    let state = if state == RawState::Downloading && size > 0 && done >= size {
        RawState::Seeding
    } else {
        state
    };
    Ok(Some((
        gid,
        RawTorrent {
            hash,
            name,
            label,
            save_path: dir,
            size_bytes: size,
            downloaded_bytes: done.min(size),
            state,
            download_rate: download,
            upload_rate: number(entry, "uploadSpeed"),
            eta_seconds: (download > 0 && size > done).then(|| (size - done) / download),
            ratio: if done == 0 {
                0.0
            } else {
                number(entry, "uploadLength") as f64 / done as f64
            },
            seeding_seconds: None,
        },
    )))
}

impl Wire for Aria2Wire<'_, '_> {
    fn redact(&self, value: String) -> String {
        match &self.secret {
            Some(secret) if !secret.is_empty() => value.replace(secret.as_str(), "[redacted]"),
            _ => value,
        }
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        let version = self.call("aria2.getVersion", vec![]).await?;
        let raw = version
            .get("version")
            .and_then(Value::as_str)
            .ok_or(QbitError::InvalidResponse)?
            .trim();
        if raw.is_empty() || raw.len() > 64 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(QbitError::InvalidResponse);
        }
        let mut parts = raw.split('.').map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
        });
        match (
            parts.next().and_then(|p| p.ok()),
            parts.next().and_then(|p| p.ok()),
        ) {
            (Some(major), Some(minor)) if (major, minor) >= MIN_VERSION => {}
            (Some(_), Some(_)) => return Err(QbitError::UnsupportedVersion),
            _ => return Err(QbitError::InvalidResponse),
        }
        let options = self.call("aria2.getGlobalOption", vec![]).await?;
        self.default_dir = normalize(options.get("dir").and_then(Value::as_str).unwrap_or(""));
        Ok(WireInfo {
            version: raw.to_owned(),
            default_dir: self.default_dir.clone(),
            // A label is a subfolder of the default directory and needs no declaration.
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
        let gid = self.gid(hash).await?;
        let dir = self.get(hash).await?.ok_or(QbitError::NotFound)?.save_path;
        let status = self
            .call("aria2.tellStatus", vec![json!(gid), json!(["files"])])
            .await?;
        let files = status
            .get("files")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if files.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        files
            .iter()
            .map(|f| {
                let path = normalize(f.get("path").and_then(Value::as_str).unwrap_or(""));
                // aria2 reports absolute paths; the engine wants them relative to the directory.
                let relative = path
                    .strip_prefix(&format!("{dir}/"))
                    .ok_or(QbitError::InvalidResponse)?
                    .to_owned();
                Ok(RawFile {
                    path: relative,
                    size_bytes: number(f, "length"),
                    downloaded_bytes: number(f, "completedLength"),
                    wanted: f.get("selected").and_then(Value::as_str) != Some("false"),
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        if request.label.is_empty()
            || !request.label.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
            || self.default_dir.is_empty()
        {
            return Err(QbitError::InvalidRequest);
        }
        let mut options = json!({"dir": self.folder(request.label)});
        if request.paused {
            options["pause"] = json!("true");
        }
        let gid = match request.source {
            AddSource::Magnet(uri) => {
                self.call("aria2.addUri", vec![json!([uri]), options])
                    .await?
            }
            AddSource::Torrent { bytes, .. } => {
                self.call(
                    "aria2.addTorrent",
                    vec![
                        json!(crate::providers::transmission::base64(bytes)),
                        json!([]),
                        options,
                    ],
                )
                .await?
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        };
        let gid = gid.as_str().ok_or(QbitError::InvalidResponse)?.to_owned();
        if request.first {
            // Only waiting or paused downloads can be moved; an already active one is at the
            // front in effect, so a refusal is not an error.
            let _ = self
                .call(
                    "aria2.changePosition",
                    vec![json!(gid), json!(0), json!("POS_SET")],
                )
                .await;
        }
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, _hash: &str, _label: &str, _first: bool) -> Result<()> {
        // A download in another directory cannot be moved over RPC; never claim it.
        Err(QbitError::UnsupportedFeature)
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        let gid = json!(self.gid(hash).await?);
        match action {
            Control::Pause => self.call("aria2.pause", vec![gid]).await,
            Control::Resume => self.call("aria2.unpause", vec![gid]).await,
            // aria2 cannot delete downloaded data over RPC.
            Control::Remove {
                delete_files: false,
            } => {
                match self.call("aria2.remove", vec![gid.clone()]).await {
                    Ok(_) => {}
                    // Already stopped: only its result entry is left.
                    Err(QbitError::Rejected) => {}
                    Err(error) => return Err(error),
                }
                self.call("aria2.removeDownloadResult", vec![gid]).await
            }
            Control::Priority { first } => {
                self.call(
                    "aria2.changePosition",
                    vec![
                        gid,
                        json!(if *first { 0 } else { 0 }),
                        json!(if *first { "POS_SET" } else { "POS_END" }),
                    ],
                )
                .await
            }
            _ => return Err(QbitError::UnsupportedFeature),
        }
        .map(|_| ())
    }
}
