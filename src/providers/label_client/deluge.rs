//! Deluge Web UI JSON-RPC wire (`POST <endpoint>/json`), cookie session authenticated with the
//! Web UI password. Labels come from Deluge's Label plugin; torrents are identified by info hash.
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

const MIN_VERSION: (u32, u32) = (1, 3);
const MAX_COOKIE_BYTES: usize = 512;
const MAX_HOSTS: usize = 16;
const MAX_LABELS: usize = 4096;
const STATUS_KEYS: [&str; 13] = [
    "name",
    "state",
    "progress",
    "total_wanted",
    "total_done",
    "save_path",
    "download_payload_rate",
    "upload_payload_rate",
    "eta",
    "ratio",
    "seeding_time",
    "label",
    "is_finished",
];

pub(crate) struct DelugeWire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    url: String,
    password: String,
    cookie: Option<String>,
    request_id: u64,
    labels_supported: bool,
}
impl<'o, 'a> DelugeWire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        // The Web UI always has a password; there is no anonymous mode to fall back to.
        let Some(Credentials::ApiKey { api_key }) = credentials else {
            return Err(QbitError::InvalidRequest);
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        let path = format!("{}/json", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self {
            operation,
            url: url.into(),
            password: api_key.clone(),
            cookie: None,
            request_id: 0,
            labels_supported: false,
        })
    }

    /// One JSON-RPC call; a server `error` object is a rejection and is never reflected.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id = self
            .request_id
            .checked_add(1)
            .ok_or(QbitError::InvalidRequest)?;
        let body =
            serde_json::to_vec(&json!({"method": method, "params": params, "id": self.request_id}))
                .map_err(|_| QbitError::InvalidRequest)?;
        let mut headers = Vec::new();
        if let Some(cookie) = &self.cookie {
            headers.push(("Cookie".to_owned(), cookie.clone()));
        }
        let response = self
            .operation
            .request(&self.url, &[], &headers, HttpRequestBody::Json(body))
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(if response.status == 404 || response.status >= 500 {
                QbitError::Http(HttpError::Transport)
            } else {
                QbitError::InvalidResponse
            });
        }
        if method == "auth.login" {
            // Only the session cookie's name=value pair is kept; attributes are not replayed.
            let pair = response
                .set_cookies
                .iter()
                .filter_map(|c| c.split(';').next())
                .find(|c| c.starts_with("_session_id="))
                .map(str::trim);
            self.cookie = pair
                .filter(|p| {
                    p.len() <= MAX_COOKIE_BYTES
                        && p.bytes().all(|b| b.is_ascii_graphic() && b != b',')
                })
                .map(str::to_owned);
        }
        let mut value: Value =
            serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)?;
        if let Some(id) = value.get("id") {
            if id.as_u64() != Some(self.request_id) {
                return Err(QbitError::InvalidResponse);
            }
        }
        if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
            // Deluge answers an expired session with code 1; surface it as authentication.
            if error.get("code").and_then(Value::as_i64) == Some(1) {
                return Err(HttpError::Authentication.into());
            }
            return Err(QbitError::Rejected);
        }
        value
            .get_mut("result")
            .map(Value::take)
            .ok_or(QbitError::InvalidResponse)
    }

    async fn login(&mut self) -> Result<()> {
        let result = self
            .call("auth.login", json!([self.password.clone()]))
            .await?;
        if result.as_bool() != Some(true) || self.cookie.is_none() {
            return Err(HttpError::Authentication.into());
        }
        Ok(())
    }

    /// The Web UI proxies one daemon; attach to the first known host when it is not connected.
    async fn ensure_daemon(&mut self) -> Result<()> {
        if self.call("web.connected", json!([])).await?.as_bool() == Some(true) {
            return Ok(());
        }
        let hosts = self.call("web.get_hosts", json!([])).await?;
        let hosts = hosts.as_array().ok_or(QbitError::InvalidResponse)?;
        if hosts.len() > MAX_HOSTS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        let id = hosts
            .first()
            .and_then(|h| h.get(0))
            .and_then(Value::as_str)
            .ok_or(QbitError::Http(HttpError::Transport))?
            .to_owned();
        self.call("web.connect", json!([id])).await?;
        if self.call("web.connected", json!([])).await?.as_bool() != Some(true) {
            return Err(HttpError::Transport.into());
        }
        Ok(())
    }

    async fn version(&mut self) -> Result<String> {
        let value = match self.call("daemon.get_version", json!([])).await {
            Ok(value) => value,
            // Deluge 1.x named it `daemon.info`.
            Err(QbitError::Rejected) => self.call("daemon.info", json!([])).await?,
            Err(error) => return Err(error),
        };
        let raw = value.as_str().ok_or(QbitError::InvalidResponse)?.trim();
        if raw.is_empty() || raw.len() > 64 || !raw.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(QbitError::InvalidResponse);
        }
        let mut parts = raw.split('.').map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
        });
        let major = parts.next().and_then(|p| p.ok());
        let minor = parts.next().and_then(|p| p.ok());
        match (major, minor) {
            (Some(major), Some(minor)) if (major, minor) >= MIN_VERSION => Ok(raw.to_owned()),
            (Some(_), Some(_)) => Err(QbitError::UnsupportedVersion),
            _ => Err(QbitError::InvalidResponse),
        }
    }

    async fn declared_labels(&mut self) -> Result<Option<Vec<String>>> {
        match self.call("label.get_labels", json!([])).await {
            Ok(value) => {
                let list = value.as_array().ok_or(QbitError::InvalidResponse)?;
                if list.len() > MAX_LABELS {
                    return Err(HttpError::ResponseTooLarge.into());
                }
                Ok(Some(
                    list.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                ))
            }
            // A disabled Label plugin leaves the method undefined.
            Err(QbitError::Rejected) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn statuses(&mut self, filter: Value) -> Result<Vec<RawTorrent>> {
        let value = self
            .call("core.get_torrents_status", json!([filter, STATUS_KEYS]))
            .await?;
        let map = value.as_object().ok_or(QbitError::InvalidResponse)?;
        if map.len() > MAX_LISTED_TORRENTS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        map.iter().map(|(hash, v)| torrent(hash, v)).collect()
    }
}

fn torrent(hash: &str, value: &Value) -> Result<RawTorrent> {
    if !valid_hash(hash) {
        return Err(QbitError::InvalidResponse);
    }
    let text = |key: &str, limit: usize| -> Result<String> {
        let s = value.get(key).and_then(Value::as_str).unwrap_or("");
        if s.len() > limit || s.chars().any(char::is_control) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(s.to_owned())
    };
    let unsigned = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0)
    };
    let size = unsigned("total_wanted") as u64;
    let progress = unsigned("progress");
    let downloaded = if progress >= 100.0 {
        size
    } else {
        (unsigned("total_done") as u64).min(size)
    };
    let state = match value.get("state").and_then(Value::as_str).unwrap_or("") {
        "Downloading" if size == 0 => RawState::Metadata,
        "Downloading" => RawState::Downloading,
        "Seeding" => RawState::Seeding,
        "Paused" => RawState::Paused,
        "Queued" => RawState::Queued,
        "Allocating" | "Checking" | "Moving" => RawState::Checking,
        "Error" => RawState::Error,
        _ => RawState::Unknown,
    };
    let eta = unsigned("eta");
    let seeding = unsigned("seeding_time");
    Ok(RawTorrent {
        hash: hash.to_owned(),
        name: text("name", 1024)?,
        label: text("label", 256)?,
        save_path: text("save_path", 4096)?,
        size_bytes: size,
        downloaded_bytes: downloaded,
        state,
        download_rate: unsigned("download_payload_rate") as u64,
        upload_rate: unsigned("upload_payload_rate") as u64,
        eta_seconds: (eta > 0.0).then_some(eta as u64),
        ratio: unsigned("ratio"),
        seeding_seconds: (seeding > 0.0).then_some(seeding as u64),
    })
}

impl Wire for DelugeWire<'_, '_> {
    fn redact(&self, value: String) -> String {
        if self.password.is_empty() {
            value
        } else {
            value.replace(self.password.as_str(), "[redacted]")
        }
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        self.cookie = None;
        self.login().await?;
        self.ensure_daemon().await?;
        let version = self.version().await?;
        let default_dir = self
            .call("core.get_config_value", json!(["download_location"]))
            .await?
            .as_str()
            .unwrap_or("")
            .to_owned();
        self.labels_supported = self.declared_labels().await?.is_some();
        Ok(WireInfo {
            version,
            default_dir,
            labels_supported: self.labels_supported,
        })
    }
    async fn list(&mut self, label: Option<&str>) -> Result<Vec<RawTorrent>> {
        let filter = match label {
            Some(label) => json!({"label": label}),
            None => json!({}),
        };
        self.statuses(filter).await
    }
    async fn get(&mut self, hash: &str) -> Result<Option<RawTorrent>> {
        let mut found = self.statuses(json!({"id": [hash]})).await?;
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            _ => Err(QbitError::InvalidResponse),
        }
    }
    async fn files(&mut self, hash: &str) -> Result<Vec<RawFile>> {
        let value = self
            .call(
                "core.get_torrent_status",
                json!([hash, ["files", "file_progress", "file_priorities"]]),
            )
            .await?;
        let files = value
            .get("files")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if files.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        let progress = value.get("file_progress").and_then(Value::as_array);
        let priorities = value.get("file_priorities").and_then(Value::as_array);
        files
            .iter()
            .enumerate()
            .map(|(index, f)| {
                let size = f
                    .get("size")
                    .and_then(Value::as_u64)
                    .ok_or(QbitError::InvalidResponse)?;
                let fraction = progress
                    .and_then(|p| p.get(index))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                Ok(RawFile {
                    path: f
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or(QbitError::InvalidResponse)?
                        .to_owned(),
                    size_bytes: size,
                    downloaded_bytes: (size as f64 * fraction).round() as u64,
                    wanted: priorities
                        .and_then(|p| p.get(index))
                        .and_then(Value::as_i64)
                        .is_none_or(|p| p > 0),
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, label: &str) -> Result<()> {
        let existing = self
            .declared_labels()
            .await?
            .ok_or(QbitError::UnsupportedFeature)?;
        if existing.iter().any(|l| l == label) {
            return Ok(());
        }
        self.call("label.add", json!([label])).await?;
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        let options = json!({"add_paused": request.paused});
        let added = match request.source {
            AddSource::Magnet(uri) => {
                self.call("core.add_torrent_magnet", json!([uri, options]))
                    .await?
            }
            AddSource::Torrent { filename, bytes } => {
                let name: String = filename
                    .chars()
                    .filter(|c| c.is_ascii_graphic() && !matches!(c, '/' | '\\'))
                    .take(128)
                    .collect();
                self.call(
                    "core.add_torrent_file",
                    json!([name, crate::providers::transmission::base64(bytes), options]),
                )
                .await?
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        };
        // Older daemons answer null for a torrent they already hold.
        let Some(id) = added.as_str() else {
            return Ok(AddReply::Duplicate);
        };
        if id != request.hash {
            return Err(QbitError::InvalidResponse);
        }
        // The torrent now exists; a failure to label or place it must be reconciled, never read
        // as a plain rejection of the add.
        self.call("label.set_torrent", json!([request.hash, request.label]))
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
        if request.first {
            self.call("core.queue_top", json!([[request.hash]]))
                .await
                .map_err(|_| QbitError::MutationUnknown)?;
        }
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, hash: &str, label: &str, first: bool) -> Result<()> {
        self.call("label.set_torrent", json!([hash, label])).await?;
        if first {
            self.call("core.queue_top", json!([[hash]])).await?;
        }
        Ok(())
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        match action {
            Control::Pause => self.call("core.pause_torrent", json!([[hash]])).await,
            Control::Resume => self.call("core.resume_torrent", json!([[hash]])).await,
            Control::Remove { delete_files } => {
                self.call("core.remove_torrent", json!([hash, delete_files]))
                    .await
            }
            Control::Priority { first: true } => self.call("core.queue_top", json!([[hash]])).await,
            Control::Priority { first: false } => {
                self.call("core.queue_bottom", json!([[hash]])).await
            }
            _ => return Err(QbitError::UnsupportedFeature),
        }
        .map(|_| ())
    }
}
