//! Synology Download Station wire (`<endpoint>/webapi/...`): `SYNO.API.Auth` session ids and the
//! `SYNO.DownloadStation.Task`/`Info` APIs, every call a form POST so credentials and the session
//! id never appear in URLs. Download Station reports no info hash, so identity and domain are
//! carried by the task's destination: `<default destination>/<label>/<info hash>`. A task whose
//! magnet URI names a hash but whose destination is elsewhere is reported unlabelled, which the
//! engine treats as a scope conflict. Download Station rejects a task whose destination does not
//! exist (task error 403), so that folder is provisioned through File Station before the add.
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
use std::{collections::BTreeMap, time::Duration};

const SESSION: &str = "DownloadStation";
const AUTH_PATH: &str = "/webapi/auth.cgi";
const TASK_PATH: &str = "/webapi/DownloadStation/task.cgi";
const INFO_PATH: &str = "/webapi/DownloadStation/info.cgi";
const ENTRY_PATH: &str = "/webapi/entry.cgi";
const FOLDER_API: &str = "SYNO.FileStation.CreateFolder";
const FOLDER_API_VERSION: u32 = 2;
const MAX_SEGMENT_BYTES: usize = 255;
const MAX_SID_BYTES: usize = 256;
const VISIBLE_ATTEMPTS: u32 = 15;
const VISIBLE_INTERVAL: Duration = Duration::from_millis(200);
// Synology API error codes (common and task APIs).
const SESSION_ERRORS: [i64; 5] = [105, 106, 107, 119, 120];
const NO_SUCH_TASK: i64 = 404;
// File Station: the requested API, method or version does not exist on this DSM.
const FOLDER_API_UNAVAILABLE: [i64; 3] = [102, 103, 104];
// File Station: this account may not create folders there (403-405 invalid user/group for the
// operation, 407 operation not permitted). 105 is already a session/permission error above.
const FOLDER_DENIED: [i64; 4] = [403, 404, 405, 407];

/// Maps a non-session Synology error code of the task and info APIs.
fn task_error(code: i64) -> QbitError {
    if code == NO_SUCH_TASK {
        QbitError::NotFound
    } else {
        QbitError::InvalidResponse
    }
}
/// Maps a non-session error code of File Station's `CreateFolder`. Everything else (illegal
/// name or path, read-only or full volume, 1100/1101 creation failures) is an unusable reply.
fn folder_error(code: i64) -> QbitError {
    if FOLDER_API_UNAVAILABLE.contains(&code) {
        QbitError::UnsupportedFeature
    } else if FOLDER_DENIED.contains(&code) {
        HttpError::Authentication.into()
    } else {
        QbitError::InvalidResponse
    }
}
/// One folder name below the shared folders. The engine already restricts labels and hashes;
/// this guards the string that becomes a File Station path, including the destination read from
/// the NAS, so no segment can climb out of or alias the configured destination.
fn safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_SEGMENT_BYTES
        && segment != "."
        && segment != ".."
        && !segment.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

pub(crate) struct DownloadStationWire<'o, 'a> {
    operation: &'o HttpOperation<'a>,
    base: String,
    username: String,
    password: String,
    sid: Option<String>,
    /// Default download destination, without leading or trailing slash (a shared folder path).
    root: String,
    /// info hash -> task id of the last listing, for controls.
    ids: BTreeMap<String, String>,
}
impl<'o, 'a> DownloadStationWire<'o, 'a> {
    pub(crate) fn new(
        operation: &'o HttpOperation<'a>,
        endpoint: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Self> {
        let Some(Credentials::UsernamePassword { username, password }) = credentials else {
            return Err(QbitError::InvalidRequest);
        };
        url::Url::parse(endpoint).map_err(|_| QbitError::InvalidRequest)?;
        Ok(Self {
            operation,
            base: endpoint.trim_end_matches('/').to_owned(),
            username: username.clone(),
            password: password.clone(),
            sid: None,
            root: String::new(),
            ids: BTreeMap::new(),
        })
    }

    /// One API call; returns the `data` member of a successful reply.
    async fn call(
        &mut self,
        path: &str,
        api: &str,
        version: u32,
        method: &str,
        extra: &[(&str, String)],
    ) -> Result<Value> {
        self.call_mapped(path, api, version, method, extra, task_error)
            .await
    }
    /// [`Self::call`] with the API's own mapping for error codes that are not session errors.
    async fn call_mapped(
        &mut self,
        path: &str,
        api: &str,
        version: u32,
        method: &str,
        extra: &[(&str, String)],
        classify: fn(i64) -> QbitError,
    ) -> Result<Value> {
        let mut form = vec![
            ("api".to_owned(), api.to_owned()),
            ("version".to_owned(), version.to_string()),
            ("method".to_owned(), method.to_owned()),
        ];
        if let Some(sid) = &self.sid {
            form.push(("_sid".to_owned(), sid.clone()));
        }
        form.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        let response = self
            .operation
            .request(
                &format!("{}{path}", self.base),
                &[],
                &[],
                HttpRequestBody::Form(form),
            )
            .await?;
        self.parse_with(response.status, &response.body, classify)
    }
    fn parse(&self, status: u16, body: &[u8]) -> Result<Value> {
        self.parse_with(status, body, task_error)
    }
    fn parse_with(
        &self,
        status: u16,
        body: &[u8],
        classify: fn(i64) -> QbitError,
    ) -> Result<Value> {
        if matches!(status, 401 | 403) {
            return Err(HttpError::Authentication.into());
        }
        if !(200..300).contains(&status) {
            return Err(if status == 404 || status >= 500 {
                QbitError::Http(HttpError::Transport)
            } else {
                QbitError::InvalidResponse
            });
        }
        let value: Value = serde_json::from_slice(body).map_err(|_| QbitError::InvalidResponse)?;
        if value.get("success").and_then(Value::as_bool) == Some(true) {
            return Ok(value.get("data").cloned().unwrap_or(Value::Null));
        }
        let code = value
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64)
            .ok_or(QbitError::InvalidResponse)?;
        Err(if SESSION_ERRORS.contains(&code) {
            HttpError::Authentication.into()
        } else {
            classify(code)
        })
    }
    async fn login(&mut self) -> Result<()> {
        let form = vec![
            ("api".to_owned(), "SYNO.API.Auth".to_owned()),
            ("version".to_owned(), "6".to_owned()),
            ("method".to_owned(), "login".to_owned()),
            ("account".to_owned(), self.username.clone()),
            ("passwd".to_owned(), self.password.clone()),
            ("session".to_owned(), SESSION.to_owned()),
            ("format".to_owned(), "sid".to_owned()),
        ];
        let response = self
            .operation
            .request(
                &format!("{}{AUTH_PATH}", self.base),
                &[],
                &[],
                HttpRequestBody::Form(form),
            )
            .await?;
        // Login failures (wrong account or password, a second factor, a disabled account) are
        // all answered with error codes 400-410 and mean the credentials cannot be used.
        let data = match self.parse(response.status, &response.body) {
            Err(QbitError::InvalidResponse) => return Err(HttpError::Authentication.into()),
            other => other?,
        };
        let sid = data
            .get("sid")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= MAX_SID_BYTES
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            .ok_or(QbitError::InvalidResponse)?;
        self.sid = Some(sid.to_owned());
        Ok(())
    }
    async fn tasks(&mut self) -> Result<Vec<(String, RawTorrent)>> {
        let data = self
            .call(
                TASK_PATH,
                "SYNO.DownloadStation.Task",
                1,
                "list",
                &[
                    ("offset", "0".to_owned()),
                    ("limit", MAX_LISTED_TORRENTS.to_string()),
                    ("additional", "detail,transfer".to_owned()),
                ],
            )
            .await?;
        let rows = data
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if rows.len() > MAX_LISTED_TORRENTS
            || data.get("total").and_then(Value::as_u64).unwrap_or(0) as usize > MAX_LISTED_TORRENTS
        {
            return Err(HttpError::ResponseTooLarge.into());
        }
        let mut out = Vec::new();
        for row in rows {
            if let Some(task) = task(row, &self.root)? {
                out.push(task);
            }
        }
        self.ids = out
            .iter()
            .map(|(id, t)| (t.hash.clone(), id.clone()))
            .collect();
        Ok(out)
    }
    async fn id_of(&mut self, hash: &str) -> Result<String> {
        if let Some(id) = self.ids.get(hash) {
            return Ok(id.clone());
        }
        self.tasks().await?;
        self.ids.get(hash).cloned().ok_or(QbitError::NotFound)
    }
}

fn status_state(status: &str, size: u64, done: u64) -> RawState {
    match status {
        "hash_checking" | "extracting" => RawState::Checking,
        "waiting" | "filehosting_waiting" => RawState::Queued,
        "paused" => RawState::Paused,
        "error" => RawState::Error,
        "finished" | "seeding" | "finishing" => RawState::Seeding,
        "downloading" if size == 0 => RawState::Metadata,
        "downloading" if size > 0 && done >= size => RawState::Seeding,
        "downloading" => RawState::Downloading,
        _ => RawState::Unknown,
    }
}
/// `Some((task id, torrent))` for BitTorrent tasks whose hash can be determined.
fn task(row: &Value, root: &str) -> Result<Option<(String, RawTorrent)>> {
    let text = |v: Option<&Value>, limit: usize| -> Result<String> {
        let s = v
            .and_then(Value::as_str)
            .ok_or(QbitError::InvalidResponse)?;
        if s.len() > limit || s.chars().any(char::is_control) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(s.to_owned())
    };
    let id = text(row.get("id"), 128)?;
    let title = text(row.get("title"), 1024)?;
    let status = text(row.get("status"), 64)?;
    let detail = row.get("additional").and_then(|a| a.get("detail"));
    let destination = text(detail.and_then(|d| d.get("destination")), 4096)?
        .trim_matches('/')
        .to_owned();
    let uri = detail
        .and_then(|d| d.get("uri"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let parts: Vec<&str> = destination.split('/').collect();
    let (label, hash) = match parts.as_slice() {
        [.., label, hash]
            if !root.is_empty()
                && destination == format!("{root}/{label}/{hash}")
                && valid_hash(&hash.to_ascii_lowercase()) =>
        {
            ((*label).to_owned(), hash.to_ascii_lowercase())
        }
        _ => match magnet_hash(uri) {
            Some(hash) => (String::new(), hash),
            None => return Ok(None),
        },
    };
    let size = bytes(row.get("size"))?;
    let transfer = row.get("additional").and_then(|a| a.get("transfer"));
    let number = |key: &str| bytes(transfer.and_then(|t| t.get(key)));
    let done = number("size_downloaded")?;
    let uploaded = number("size_uploaded")?;
    let (download_rate, upload_rate) = (number("speed_download")?, number("speed_upload")?);
    Ok(Some((
        id,
        RawTorrent {
            hash,
            name: title,
            label,
            save_path: format!("/{destination}"),
            size_bytes: size,
            downloaded_bytes: if size > 0 { done.min(size) } else { 0 },
            state: status_state(&status, size, done),
            download_rate,
            upload_rate,
            eta_seconds: None,
            ratio: if size > 0 {
                uploaded as f64 / size as f64
            } else {
                0.0
            },
            seeding_seconds: None,
        },
    )))
}
/// A byte count or rate as Download Station reports it: its API guide types these as strings,
/// while some builds send JSON numbers. A missing field is zero; a present value that is neither
/// a number nor a plain decimal string is a malformed response, never a silent zero.
fn bytes(value: Option<&Value>) -> Result<u64> {
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => n.as_u64().ok_or(QbitError::InvalidResponse),
        Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().map_err(|_| QbitError::InvalidResponse)
        }
        Some(_) => Err(QbitError::InvalidResponse),
    }
}
/// Lowercase 40-character info hash of a magnet URI, if it carries one.
fn magnet_hash(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("magnet:?")?;
    let value = rest
        .split('&')
        .find_map(|p| p.strip_prefix("xt=urn:btih:"))?;
    let hash = value.to_ascii_lowercase();
    valid_hash(&hash).then_some(hash)
}

impl Wire for DownloadStationWire<'_, '_> {
    fn redact(&self, mut value: String) -> String {
        for secret in [
            Some(&self.username),
            Some(&self.password),
            self.sid.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        {
            value = value.replace(secret.as_str(), "[redacted]");
        }
        value
    }
    async fn connect(&mut self) -> Result<WireInfo> {
        self.login().await?;
        let info = self
            .call(INFO_PATH, "SYNO.DownloadStation.Info", 1, "getinfo", &[])
            .await?;
        let version = info
            .get("version_string")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control))
            .ok_or(QbitError::InvalidResponse)?
            .to_owned();
        let config = self
            .call(INFO_PATH, "SYNO.DownloadStation.Info", 1, "getconfig", &[])
            .await?;
        let destination = config
            .get("default_destination")
            .and_then(Value::as_str)
            .map(|d| d.trim_matches('/').to_owned())
            .filter(|d| !d.is_empty() && d.len() <= 4096 && !d.chars().any(char::is_control))
            .ok_or(QbitError::UnsupportedFeature)?;
        self.root = destination.clone();
        // The task listing proves the account may use Download Station.
        self.tasks().await?;
        Ok(WireInfo {
            version,
            default_dir: format!("/{destination}"),
            // A domain is a destination folder; nothing needs declaring.
            labels_supported: true,
        })
    }
    async fn list(&mut self, _label: Option<&str>) -> Result<Vec<RawTorrent>> {
        Ok(self.tasks().await?.into_iter().map(|(_, t)| t).collect())
    }
    async fn get(&mut self, hash: &str) -> Result<Option<RawTorrent>> {
        Ok(self
            .tasks()
            .await?
            .into_iter()
            .map(|(_, t)| t)
            .find(|t| t.hash == hash))
    }
    async fn files(&mut self, hash: &str) -> Result<Vec<RawFile>> {
        let id = self.id_of(hash).await?;
        let data = self
            .call(
                TASK_PATH,
                "SYNO.DownloadStation.Task",
                1,
                "getinfo",
                &[("id", id), ("additional", "file".to_owned())],
            )
            .await?;
        let files = data
            .get("tasks")
            .and_then(Value::as_array)
            .and_then(|t| t.first())
            .and_then(|t| t.get("additional"))
            .and_then(|a| a.get("file"))
            .and_then(Value::as_array)
            .ok_or(QbitError::InvalidResponse)?;
        if files.len() > MAX_FILES {
            return Err(QbitError::InvalidResponse);
        }
        files
            .iter()
            .map(|f| {
                let path = f
                    .get("filename")
                    .and_then(Value::as_str)
                    .filter(|p| p.len() <= 4096 && !p.chars().any(char::is_control))
                    .ok_or(QbitError::InvalidResponse)?
                    .trim_start_matches('/')
                    .to_owned();
                let size = bytes(f.get("size"))?;
                let done = bytes(f.get("size_downloaded"))?;
                Ok(RawFile {
                    path,
                    size_bytes: size,
                    downloaded_bytes: done.min(size),
                    wanted: f.get("priority").and_then(Value::as_str) != Some("skip"),
                })
            })
            .collect()
    }
    async fn ensure_label(&mut self, _label: &str) -> Result<()> {
        Ok(())
    }
    /// Creates `<default destination>/<label>/<info hash>` (and any missing parent) so the task
    /// can name it. `force_parent` makes an existing folder a success, so repeating this after
    /// an ambiguous attempt is harmless and never touches a sibling folder.
    async fn prepare_destination(&mut self, label: &str, hash: &str) -> Result<()> {
        if !safe_segment(label) || !valid_hash(hash) || !self.root.split('/').all(safe_segment) {
            return Err(QbitError::InvalidRequest);
        }
        let parent = format!("/{}/{label}", self.root);
        let data = self
            .call_mapped(
                ENTRY_PATH,
                FOLDER_API,
                FOLDER_API_VERSION,
                "create",
                &[
                    ("folder_path", parent.clone()),
                    ("name", hash.to_owned()),
                    ("force_parent", "true".to_owned()),
                ],
                folder_error,
            )
            .await?;
        // The reply must describe the folder that was asked for, never another location.
        let expected = format!("{parent}/{hash}");
        let created = data
            .get("folders")
            .and_then(Value::as_array)
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.get("path"))
            .and_then(Value::as_str);
        if created.is_some_and(|path| path != expected) {
            return Err(QbitError::InvalidResponse);
        }
        Ok(())
    }
    async fn add(&mut self, request: &AddRequest<'_>) -> Result<AddReply> {
        let destination = format!("{}/{}/{}", self.root, request.label, request.hash);
        match request.source {
            AddSource::Magnet(uri) => {
                self.call(
                    TASK_PATH,
                    "SYNO.DownloadStation.Task",
                    1,
                    "create",
                    &[("uri", uri.clone()), ("destination", destination)],
                )
                .await?;
            }
            AddSource::Torrent { bytes, .. } => {
                let mut fields = vec![
                    ("api".to_owned(), "SYNO.DownloadStation.Task".to_owned()),
                    ("version".to_owned(), "1".to_owned()),
                    ("method".to_owned(), "create".to_owned()),
                    ("destination".to_owned(), destination),
                ];
                if let Some(sid) = &self.sid {
                    fields.push(("_sid".to_owned(), sid.clone()));
                }
                let response = self
                    .operation
                    .request(
                        &format!("{}{TASK_PATH}", self.base),
                        &[],
                        &[],
                        HttpRequestBody::Multipart {
                            fields,
                            file_name: "release.torrent".to_owned(),
                            field_name: "file".to_owned(),
                            file: bytes.clone(),
                        },
                    )
                    .await?;
                self.parse(response.status, &response.body)?;
            }
            AddSource::Url(_) => return Err(QbitError::InvalidRequest),
        }
        // Download Station creates tasks asynchronously; the engine verifies them once listed.
        let mut listed = false;
        for _ in 0..VISIBLE_ATTEMPTS {
            if self.get(request.hash).await?.is_some() {
                listed = true;
                break;
            }
            tokio::time::sleep(VISIBLE_INTERVAL).await;
        }
        if request.paused && listed {
            let id = self.id_of(request.hash).await?;
            self.call(
                TASK_PATH,
                "SYNO.DownloadStation.Task",
                1,
                "pause",
                &[("id", id)],
            )
            .await
            .map_err(|_| QbitError::MutationUnknown)?;
        }
        Ok(AddReply::Added)
    }
    async fn adopt(&mut self, _hash: &str, _label: &str, _first: bool) -> Result<()> {
        // The destination is part of the create request, so an unlabelled lost add cannot occur.
        Err(QbitError::UnsupportedFeature)
    }
    async fn control(&mut self, hash: &str, action: &Control) -> Result<()> {
        let (method, extra): (&str, Vec<(&str, String)>) = match action {
            Control::Pause => ("pause", vec![]),
            Control::Resume => ("resume", vec![]),
            Control::Remove {
                delete_files: false,
            } => ("delete", vec![("force_complete", "false".to_owned())]),
            // Download Station cannot delete downloaded data through its API.
            _ => return Err(QbitError::UnsupportedFeature),
        };
        let id = self.id_of(hash).await?;
        let mut params = vec![("id", id)];
        params.extend(extra);
        self.call(TASK_PATH, "SYNO.DownloadStation.Task", 1, method, &params)
            .await
            .map(|_| ())
    }
    async fn finish(&mut self) {
        if self.sid.is_some() {
            // Best effort: an unreachable NAS leaves the session to expire on its own.
            let _ = self
                .call(
                    AUTH_PATH,
                    "SYNO.API.Auth",
                    1,
                    "logout",
                    &[("session", SESSION.to_owned())],
                )
                .await;
            self.sid = None;
        }
    }
}
