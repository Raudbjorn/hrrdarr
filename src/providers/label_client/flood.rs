//! Flood REST wire (`<endpoint>/api/...`). Flood fronts rTorrent, qBittorrent, Transmission or
//! Deluge; torrents are identified by info hash and a domain is a Flood tag. Authentication is the
//! `jwt` cookie issued by `/api/auth/authenticate` (Flood can also run with authentication
//! disabled, in which case no credentials are configured).
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
use std::time::Duration;

const MAX_COOKIE_BYTES: usize = 2048;
const MAX_TAGS: usize = 64;
const VISIBLE_ATTEMPTS: u32 = 15;
const VISIBLE_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) struct FloodWire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    base: String,
    login: Option<(String, String)>,
    secrets: Vec<String>,
    cookie: Option<String>,
    /// Configured domain tags: of several tags on a torrent the one naming a domain wins.
    labels: Vec<String>,
}
impl<'o, 'a> FloodWire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
        labels: Vec<String>,
    ) -> Result<Self> {
        let login = match credentials {
            None => None,
            Some(Credentials::UsernamePassword { username, password }) => {
                Some((username.clone(), password.clone()))
            }
            Some(_) => return Err(QbitError::InvalidRequest),
        };
        url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        Ok(Self {
            operation,
            base: endpoint.trim_end_matches('/').to_owned(),
            secrets: login
                .iter()
                .flat_map(|(u, p)| [u.clone(), p.clone()])
                .filter(|s| !s.is_empty())
                .collect(),
            login,
            cookie: None,
            labels,
        })
    }

    fn headers(&self) -> Vec<(String, String)> {
        self.cookie
            .iter()
            .map(|c| ("Cookie".to_owned(), c.clone()))
            .collect()
    }
    fn check(status: u16) -> Result<()> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        Err(if status == 404 || status >= 500 {
            QbitError::Http(HttpError::Transport)
        } else {
            QbitError::InvalidResponse
        })
    }
    async fn authenticate(&mut self) -> Result<()> {
        let Some((username, password)) = self.login.clone() else {
            return Ok(());
        };
        let body = serde_json::to_vec(&json!({"username":username,"password":password}))
            .map_err(|_| QbitError::InvalidRequest)?;
        let response = self
            .operation
            .request(
                &format!("{}/api/auth/authenticate", self.base),
                &[],
                &[],
                HttpRequestBody::Json(body),
            )
            .await?;
        if matches!(response.status, 400 | 401 | 403) {
            return Err(HttpError::Authentication.into());
        }
        Self::check(response.status)?;
        self.cookie = response
            .set_cookies
            .iter()
            .filter_map(|c| c.split(';').next())
            .map(str::trim)
            .find(|c| c.starts_with("jwt="))
            .filter(|c| {
                c.len() <= MAX_COOKIE_BYTES && c.bytes().all(|b| b.is_ascii_graphic() && b != b',')
            })
            .map(str::to_owned);
        // With authentication enabled the reply must carry the session cookie.
        if self.cookie.is_none() {
            return Err(HttpError::Authentication.into());
        }
        Ok(())
    }
    async fn get_json(&mut self, path: &str) -> Result<Value> {
        let response = self
            .operation
            .request(
                &format!("{}{path}", self.base),
                &[],
                &self.headers(),
                HttpRequestBody::Empty,
            )
            .await?;
        if matches!(response.status, 401 | 403) {
            return Err(HttpError::Authentication.into());
        }
        Self::check(response.status)?;
        serde_json::from_slice(&response.body).map_err(|_| QbitError::InvalidResponse)
    }
    async fn post(&mut self, path: &str, body: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&body).map_err(|_| QbitError::InvalidRequest)?;
        let response = self
            .operation
            .request(
                &format!("{}{path}", self.base),
                &[],
                &self.headers(),
                HttpRequestBody::Json(bytes),
            )
            .await?;
        if matches!(response.status, 401 | 403) {
            return Err(HttpError::Authentication.into());
        }
        Self::check(response.status)
    }
    async fn all(&mut self) -> Result<Vec<RawTorrent>> {
        let value = self.get_json("/api/torrents").await?;
        let map = value
            .get("torrents")
            .and_then(Value::as_object)
            .ok_or(QbitError::InvalidResponse)?;
        if map.len() > MAX_LISTED_TORRENTS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        map.values().map(|t| torrent(t, &self.labels)).collect()
    }
}

fn torrent(row: &Value, labels: &[String]) -> Result<RawTorrent> {
    let text = |key: &str, limit: usize| -> Result<String> {
        let s = row
            .get(key)
            .and_then(Value::as_str)
            .ok_or(QbitError::InvalidResponse)?;
        if s.len() > limit || s.chars().any(char::is_control) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(s.to_owned())
    };
    let int = |key: &str| row.get(key).and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
    let hash = text("hash", 64)?.to_ascii_lowercase();
    if !valid_hash(&hash) {
        return Err(QbitError::InvalidResponse);
    }
    let name = text("name", 1024)?;
    let size = int("sizeBytes");
    let done = int("bytesDone").min(size);
    let tags: Vec<&str> = row
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| t.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if tags.len() > MAX_TAGS
        || tags
            .iter()
            .any(|t| t.len() > 256 || t.chars().any(char::is_control))
    {
        return Err(QbitError::InvalidResponse);
    }
    let label = tags
        .iter()
        .find(|t| labels.iter().any(|l| l.eq_ignore_ascii_case(t)))
        .or_else(|| tags.first())
        .map_or(String::new(), |t| (*t).to_owned());
    let status: Vec<&str> = row
        .get("status")
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let has = |s: &str| status.contains(&s);
    let state = if has("checking") {
        RawState::Checking
    } else if has("error") {
        RawState::Error
    } else if size == 0 {
        RawState::Metadata
    } else if has("stopped") {
        RawState::Paused
    } else if has("complete") || has("seeding") || done >= size {
        RawState::Seeding
    } else if has("downloading") {
        RawState::Downloading
    } else {
        RawState::Stalled
    };
    let mut save_path = text("directory", 4096)?.replace('\\', "/");
    // For a multi-file torrent the directory is the torrent's own folder; the engine's file paths
    // are relative to the folder that contains it.
    if let Some((parent, last)) = save_path.rsplit_once('/') {
        if last == name && !parent.is_empty() {
            save_path = parent.to_owned();
        }
    }
    let eta = row.get("eta").and_then(Value::as_i64).unwrap_or(-1);
    Ok(RawTorrent {
        hash,
        name,
        label,
        save_path,
        size_bytes: size,
        downloaded_bytes: done,
        state,
        download_rate: int("downRate"),
        upload_rate: int("upRate"),
        eta_seconds: (eta > 0).then_some(eta as u64),
        ratio: row
            .get("ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0),
        seeding_seconds: None,
    })
}

impl Wire for FloodWire<'_, '_> {
    fn redact(&self, mut value: String) -> String {
        for secret in &self.secrets {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        if let Some(cookie) = &self.cookie {
            if let Some((_, token)) = cookie.split_once('=') {
                value = value.replace(token, "[redacted]");
            }
        }
        value
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        self.authenticate().await?;
        let connection = self.get_json("/api/client/connection-test").await?;
        if connection.get("isConnected").and_then(Value::as_bool) != Some(true) {
            // Flood is up but cannot reach its torrent client.
            return Err(QbitError::Http(HttpError::Transport));
        }
        let settings = self.get_json("/api/client/settings").await?;
        let default_dir = settings
            .get("directoryDefault")
            .and_then(Value::as_str)
            .filter(|d| d.len() <= 4096 && !d.chars().any(char::is_control))
            .unwrap_or("")
            .replace('\\', "/");
        // The listing proves the account may read torrents.
        self.all().await?;
        Ok(WireInfo {
            // Flood publishes no version; it is identified by its REST surface.
            version: "Flood REST API".to_owned(),
            default_dir,
            // Tags need no declaration.
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
        let torrent = self.get(hash).await?.ok_or(QbitError::NotFound)?;
        let value = self
            .get_json(&format!(
                "/api/torrents/{}/contents",
                hash.to_ascii_uppercase()
            ))
            .await?;
        let files = value.as_array().ok_or(QbitError::InvalidResponse)?;
        if files.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        let single = files.len() == 1;
        files
            .iter()
            .map(|f| {
                let name = f
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or(QbitError::InvalidResponse)?
                    .replace('\\', "/");
                let size = f
                    .get("sizeBytes")
                    .and_then(Value::as_i64)
                    .ok_or(QbitError::InvalidResponse)?
                    .max(0) as u64;
                let percent = f
                    .get("percentComplete")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0);
                // Flood's priority 0 means "do not download".
                let priority = f.get("priority").and_then(Value::as_i64).unwrap_or(1);
                Ok(RawFile {
                    path: if single || name.starts_with(&format!("{}/", torrent.name)) {
                        name
                    } else {
                        format!("{}/{name}", torrent.name)
                    },
                    size_bytes: size,
                    downloaded_bytes: (size as f64 * percent / 100.0) as u64,
                    wanted: priority > 0,
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        let start = !request.paused;
        match request.source {
            AddSource::Magnet(uri) => {
                self.post(
                    "/api/torrents/add-urls",
                    json!({"urls":[uri],"tags":[request.label],"start":start}),
                )
                .await?;
            }
            AddSource::Torrent { bytes, .. } => {
                self.post(
                    "/api/torrents/add-files",
                    json!({
                        "files":[crate::providers::transmission::base64(bytes)],
                        "tags":[request.label],
                        "start":start,
                    }),
                )
                .await?;
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        }
        // Flood hands the torrent to its client asynchronously; wait until it is listed so the
        // engine can verify the tag and paused state.
        for _ in 0..VISIBLE_ATTEMPTS {
            if self.get(request.hash).await?.is_some() {
                break;
            }
            tokio::time::sleep(VISIBLE_INTERVAL).await;
        }
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, _hash: &str, _label: &str, _first: bool) -> Result<()> {
        // The tag is part of the add request, so an unlabelled lost add cannot occur.
        Err(QbitError::UnsupportedFeature)
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        let hashes = json!({"hashes":[hash.to_ascii_uppercase()]});
        match action {
            Control::Pause => self.post("/api/torrents/stop", hashes).await,
            Control::Resume => self.post("/api/torrents/start", hashes).await,
            Control::Remove { delete_files } => {
                self.post(
                    "/api/torrents/delete",
                    json!({"hashes":[hash.to_ascii_uppercase()],"deleteData":delete_files}),
                )
                .await
            }
            // Flood changes queue order only through PATCH requests, which the provider
            // transport does not send.
            _ => Err(QbitError::UnsupportedFeature),
        }
    }
}
