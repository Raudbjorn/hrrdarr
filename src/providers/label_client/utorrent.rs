//! uTorrent Web UI wire (`<endpoint>/token.html`, `<endpoint>/?token=...&list=1` and
//! `action=...`), HTTP basic authentication plus the Web UI's token and GUID cookie. Torrents are
//! labelled with uTorrent labels and identified by info hash.
use super::{
    AddReply, AddRequest, MAX_FILES, MAX_LISTED_TORRENTS, RawFile, RawState, RawTorrent, Result,
    Wire, WireInfo, valid_hash,
};
use crate::providers::{
    Credentials,
    http::{HttpError, HttpOperation, HttpRequestBody},
    qbittorrent::{AddSource, Control, QbitError},
};
use serde_json::Value;
use std::time::Duration;

const MAX_TOKEN_BYTES: usize = 128;
const MAX_COOKIE_BYTES: usize = 512;
const MAX_SETTINGS: usize = 4096;
/// A Web UI that does not report a build cannot be the supported 3.x line.
const MIN_BUILD: i64 = 1;
const VISIBLE_ATTEMPTS: u32 = 15;
const VISIBLE_INTERVAL: Duration = Duration::from_millis(200);
// Status bit field of a torrent (documented by uTorrent's Web UI API).
const STARTED: i64 = 1;
const CHECKING: i64 = 2;
const ERROR: i64 = 16;
const PAUSED: i64 = 32;
const QUEUED: i64 = 64;

pub(crate) struct UtorrentWire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    base: String,
    authorization: String,
    secrets: Vec<String>,
    token: Option<String>,
    cookie: Option<String>,
}
impl<'o, 'a> UtorrentWire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        // The Web UI always requires a user name and password.
        let Some(Credentials::UsernamePassword { username, password }) = credentials else {
            return Err(QbitError::InvalidRequest);
        };
        // RFC 7617: the user-id cannot contain a colon.
        if username.contains(':') {
            return Err(QbitError::InvalidRequest);
        }
        url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        Ok(Self {
            operation,
            base: endpoint.trim_end_matches('/').to_owned(),
            authorization: format!(
                "Basic {}",
                crate::providers::transmission::base64(format!("{username}:{password}").as_bytes())
            ),
            secrets: [username, password]
                .into_iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect(),
            token: None,
            cookie: None,
        })
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("Authorization".to_owned(), self.authorization.clone())];
        if let Some(cookie) = &self.cookie {
            headers.push(("Cookie".to_owned(), cookie.clone()));
        }
        headers
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
    async fn login(&mut self) -> Result<()> {
        let response = self
            .operation
            .request(
                &format!("{}/token.html", self.base),
                &[],
                &self.headers(),
                HttpRequestBody::Empty,
            )
            .await?;
        Self::check(response.status)?;
        let body = std::str::from_utf8(&response.body).map_err(|_| QbitError::InvalidResponse)?;
        // The token page is `<div id='token' style='display:none;'>TOKEN</div>`.
        let token = body
            .find("id='token'")
            .or_else(|| body.find("id=\"token\""))
            .and_then(|at| body[at..].split_once('>'))
            .and_then(|(_, rest)| rest.split_once('<'))
            .map(|(token, _)| token.trim())
            .filter(|t| {
                !t.is_empty()
                    && t.len() <= MAX_TOKEN_BYTES
                    && t.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
            .ok_or(QbitError::InvalidResponse)?;
        self.token = Some(token.to_owned());
        self.cookie = response
            .set_cookies
            .iter()
            .filter_map(|c| c.split(';').next())
            .map(str::trim)
            .find(|c| c.starts_with("GUID="))
            .filter(|c| {
                c.len() <= MAX_COOKIE_BYTES && c.bytes().all(|b| b.is_ascii_graphic() && b != b',')
            })
            .map(str::to_owned);
        Ok(())
    }
    async fn json(&mut self, query: &[(&str, &str)]) -> Result<Value> {
        let text = self.get_raw(query).await?;
        serde_json::from_slice(&text).map_err(|_| QbitError::InvalidResponse)
    }
    async fn get_raw(&mut self, query: &[(&str, &str)]) -> Result<Vec<u8>> {
        let token = self.token.clone().ok_or(QbitError::InvalidRequest)?;
        let mut pairs = vec![("token".to_owned(), token)];
        pairs.extend(
            query
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        let response = self
            .operation
            .request(
                &format!("{}/", self.base),
                &pairs,
                &self.headers(),
                HttpRequestBody::Empty,
            )
            .await?;
        // An expired token or cookie is answered as a refusal of the request.
        if response.status == 400 || response.status == 401 || response.status == 403 {
            return Err(HttpError::Authentication.into());
        }
        Self::check(response.status)?;
        Ok(response.body)
    }
    async fn action(&mut self, action: &str, hash: &str, extra: &[(&str, &str)]) -> Result<()> {
        let upper = hash.to_ascii_uppercase();
        let mut query = vec![("action", action), ("hash", upper.as_str())];
        query.extend_from_slice(extra);
        // Actions answer with a JSON document (or an empty body) that carries no result.
        self.get_raw(&query).await.map(|_| ())
    }
    async fn all(&mut self) -> Result<(i64, Vec<RawTorrent>)> {
        let value = self.json(&[("list", "1")]).await?;
        let build = value
            .get("build")
            .and_then(Value::as_i64)
            .ok_or(QbitError::InvalidResponse)?;
        let rows = value
            .get("torrents")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if rows.len() > MAX_LISTED_TORRENTS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        Ok((build, rows.iter().map(torrent).collect::<Result<_>>()?))
    }
}

fn torrent(row: &Value) -> Result<RawTorrent> {
    let cells = row.as_array().ok_or(QbitError::InvalidResponse)?;
    if cells.len() < 27 {
        return Err(QbitError::InvalidResponse);
    }
    let text = |i: usize, limit: usize| -> Result<String> {
        let s = cells[i].as_str().ok_or(QbitError::InvalidResponse)?;
        if s.len() > limit || s.chars().any(char::is_control) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(s.to_owned())
    };
    let int = |i: usize| -> Result<i64> { cells[i].as_i64().ok_or(QbitError::InvalidResponse) };
    let hash = text(0, 64)?.to_ascii_lowercase();
    if !valid_hash(&hash) {
        return Err(QbitError::InvalidResponse);
    }
    let status = int(1)?;
    let name = text(2, 1024)?;
    let size = int(3)?.max(0) as u64;
    // Progress is reported in thousandths.
    let progress = int(4)?.clamp(0, 1000) as u64;
    let downloaded = if progress == 1000 {
        size
    } else {
        (size as u128 * progress as u128 / 1000) as u64
    };
    let mut save_path = text(26, 4096)?.replace('\\', "/");
    // The root download path of a torrent ends with its own name; the engine's file paths are
    // relative to the folder that contains it.
    if let Some((parent, last)) = save_path.rsplit_once('/') {
        if last == name && !parent.is_empty() {
            save_path = parent.to_owned();
        }
    }
    let state = if status & CHECKING != 0 {
        RawState::Checking
    } else if size == 0 {
        RawState::Metadata
    } else if status & ERROR != 0 {
        RawState::Error
    } else if status & PAUSED != 0 || status & STARTED == 0 && status & QUEUED == 0 {
        RawState::Paused
    } else if status & QUEUED != 0 && status & STARTED == 0 {
        RawState::Queued
    } else if progress == 1000 {
        RawState::Seeding
    } else {
        RawState::Downloading
    };
    let eta = int(10)?;
    Ok(RawTorrent {
        hash,
        name,
        label: text(11, 1024)?,
        save_path,
        size_bytes: size,
        downloaded_bytes: downloaded,
        state,
        download_rate: int(9)?.max(0) as u64,
        upload_rate: int(8)?.max(0) as u64,
        eta_seconds: (eta > 0).then_some(eta as u64),
        ratio: int(7)?.max(0) as f64 / 1000.0,
        seeding_seconds: None,
    })
}

impl Wire for UtorrentWire<'_, '_> {
    fn redact(&self, mut value: String) -> String {
        for secret in &self.secrets {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        if let Some(token) = &self.token {
            value = value.replace(token.as_str(), "[redacted]");
        }
        value
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        self.login().await?;
        let (build, _) = self.all().await?;
        if build < MIN_BUILD {
            return Err(QbitError::UnsupportedVersion);
        }
        let settings = self.json(&[("action", "getsettings")]).await?;
        let list = settings
            .get("settings")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if list.len() > MAX_SETTINGS {
            return Err(HttpError::ResponseTooLarge.into());
        }
        let setting = |name: &str| {
            list.iter()
                .filter_map(Value::as_array)
                .find(|s| s.first().and_then(Value::as_str) == Some(name))
                .and_then(|s| s.get(2))
                .and_then(Value::as_str)
                .unwrap_or("")
        };
        // Finished torrents are moved to the completed directory when that option is on.
        let default_dir = if setting("dir_completed_download_flag") == "true" {
            setting("dir_completed_download")
        } else {
            setting("dir_active_download")
        };
        Ok(WireInfo {
            version: format!("build {build}"),
            default_dir: default_dir.replace('\\', "/"),
            // Labels need no declaration or plugin.
            labels_supported: true,
        })
    }
    async fn list(&mut self, _label: Option<&str>) -> Result<Vec<RawTorrent>> {
        Ok(self.all().await?.1)
    }
    async fn get(&mut self, hash: &str) -> Result<Option<RawTorrent>> {
        Ok(self.all().await?.1.into_iter().find(|t| t.hash == hash))
    }
    async fn files(&mut self, hash: &str) -> Result<Vec<RawFile>> {
        let torrent = self.get(hash).await?.ok_or(QbitError::NotFound)?;
        let upper = hash.to_ascii_uppercase();
        let value = self
            .json(&[("action", "getfiles"), ("hash", upper.as_str())])
            .await?;
        let files = value
            .get("files")
            .and_then(Value::as_array)
            .and_then(|f| f.get(1))
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if files.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        let single = files.len() == 1
            && files[0].get(0).and_then(Value::as_str) == Some(torrent.name.as_str());
        files
            .iter()
            .map(|f| {
                let cells = f
                    .as_array()
                    .filter(|c| c.len() >= 4)
                    .ok_or(QbitError::InvalidResponse)?;
                let name = cells[0]
                    .as_str()
                    .ok_or(QbitError::InvalidResponse)?
                    .replace('\\', "/");
                let size = cells[1].as_i64().ok_or(QbitError::InvalidResponse)?.max(0) as u64;
                let done = cells[2].as_i64().ok_or(QbitError::InvalidResponse)?.max(0) as u64;
                let priority = cells[3].as_i64().ok_or(QbitError::InvalidResponse)?;
                Ok(RawFile {
                    // Paths inside a multi-file torrent are relative to its own folder.
                    path: if single || name.starts_with(&format!("{}/", torrent.name)) {
                        name
                    } else {
                        format!("{}/{name}", torrent.name)
                    },
                    size_bytes: size,
                    downloaded_bytes: done.min(size),
                    wanted: priority > 0,
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, _label: &str) -> Result<()> {
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        match request.source {
            AddSource::Magnet(uri) => {
                self.get_raw(&[("action", "add-url"), ("s", uri.as_str())])
                    .await?;
            }
            AddSource::Torrent { filename, bytes } => {
                let token = self.token.clone().ok_or(QbitError::InvalidRequest)?;
                let name: String = filename
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    .take(96)
                    .collect();
                let response = self
                    .operation
                    .request(
                        &format!("{}/", self.base),
                        &[
                            ("token".to_owned(), token),
                            ("action".to_owned(), "add-file".to_owned()),
                        ],
                        &self.headers(),
                        HttpRequestBody::Multipart {
                            fields: Vec::new(),
                            file_name: if name.is_empty() {
                                "release.torrent".to_owned()
                            } else {
                                name
                            },
                            field_name: "torrent_file".to_owned(),
                            file: bytes.clone(),
                        },
                    )
                    .await?;
                Self::check(response.status)?;
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        }
        // The label can only be applied once the torrent is listed.
        for _ in 0..VISIBLE_ATTEMPTS {
            if self.get(request.hash).await?.is_some() {
                break;
            }
            tokio::time::sleep(VISIBLE_INTERVAL).await;
        }
        // From here the torrent may exist unlabelled; failures are reconciled, not retried.
        self.action(
            "setprops",
            request.hash,
            &[("s", "label"), ("v", request.label)],
        )
        .await
        .map_err(|_| QbitError::MutationUnknown)?;
        if request.paused {
            self.action("stop", request.hash, &[])
                .await
                .map_err(|_| QbitError::MutationUnknown)?;
        }
        if request.first {
            self.action("queuetop", request.hash, &[])
                .await
                .map_err(|_| QbitError::MutationUnknown)?;
        }
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, hash: &str, label: &str, first: bool) -> Result<()> {
        self.action("setprops", hash, &[("s", "label"), ("v", label)])
            .await?;
        if first {
            self.action("queuetop", hash, &[]).await?;
        }
        Ok(())
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        match action {
            Control::Pause => self.action("pause", hash, &[]).await,
            Control::Resume => {
                self.action("unpause", hash, &[]).await?;
                self.action("start", hash, &[]).await
            }
            Control::Remove {
                delete_files: false,
            } => self.action("remove", hash, &[]).await,
            Control::Remove { delete_files: true } => self.action("removedata", hash, &[]).await,
            Control::Priority { first: true } => self.action("queuetop", hash, &[]).await,
            Control::Priority { first: false } => self.action("queuebottom", hash, &[]).await,
            _ => Err(QbitError::UnsupportedFeature),
        }
    }
}
