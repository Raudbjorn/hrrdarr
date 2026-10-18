//! Bounded transport for provider operations. URLs, headers and bodies never enter errors.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use uuid::Uuid;
const MAX_OPERATIONS: usize = 8;
const MAX_LANES: usize = 256;
const LANE_IDLE: Duration = Duration::from_secs(60);
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(100);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_RESERVE: Duration = Duration::from_secs(5);
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_FIELD_BYTES: usize = 32 * 1024;
const MAX_MULTIPART_BYTES: u64 = (MAX_BODY_BYTES + MAX_FIELD_BYTES + 16 * 1024) as u64;
// Private protocol payloads must never acquire a Debug or public serialization implementation.
pub enum HttpRequestBody {
    Empty,
    Form(Vec<(String, String)>),
    Multipart {
        fields: Vec<(String, String)>,
        file_name: String,
        field_name: String,
        file: Vec<u8>,
    },
}
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub set_cookies: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    InvalidRequest,
    /// Local state or pacing failed before a request could be observed.
    LocalUnavailable {
        timeout: bool,
    },
    Authentication,
    RateLimited {
        retry_after_seconds: Option<u32>,
    },
    Redirect,
    Transport,
    Timeout,
    Busy,
    ResponseTooLarge,
    InvalidResponse,
}
struct Lane {
    gate: Arc<Semaphore>,
    state: Mutex<LaneState>,
}
struct LaneState {
    next: Instant,
    used: Instant,
    cooldown: Option<Instant>,
}
pub struct HttpClient {
    client: reqwest::Client,
    gate: Arc<Semaphore>,
    lanes: Mutex<HashMap<Uuid, Arc<Lane>>>,
}
pub struct HttpOperation<'a> {
    client: &'a HttpClient,
    lane: Arc<Lane>,
    _global: OwnedSemaphorePermit,
    _provider: OwnedSemaphorePermit,
    deadline: Instant,
}
impl HttpClient {
    pub fn new() -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .connection_verbose(false)
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(Duration::from_secs(30))
            .user_agent("hrrdarr/0.1")
            .build()
            .map_err(|_| HttpError::Transport)?;
        Ok(Self {
            client,
            gate: Arc::new(Semaphore::new(MAX_OPERATIONS)),
            lanes: Mutex::new(HashMap::new()),
        })
    }
    pub fn operation(&self, id: Uuid) -> Result<HttpOperation<'_>, HttpError> {
        let global = self
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| HttpError::Busy)?;
        let mut lanes = self
            .lanes
            .lock()
            .map_err(|_| HttpError::LocalUnavailable { timeout: false })?;
        let now = Instant::now();
        // ponytail: bounded lanes reject excess active provider identities; raise this ceiling if measured demand needs it.
        lanes.retain(|_, lane| {
            Arc::strong_count(lane) > 1
                || lane.state.lock().map_or(true, |state| {
                    now.duration_since(state.used) < LANE_IDLE
                        || state.cooldown.is_some_and(|until| until > now)
                })
        });
        if !lanes.contains_key(&id) && lanes.len() >= MAX_LANES {
            return Err(HttpError::Busy);
        }
        let lane = lanes
            .entry(id)
            .or_insert_with(|| {
                Arc::new(Lane {
                    gate: Arc::new(Semaphore::new(1)),
                    state: Mutex::new(LaneState {
                        next: now,
                        used: now,
                        cooldown: None,
                    }),
                })
            })
            .clone();
        let provider = lane
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| HttpError::Busy)?;
        {
            let mut state = lane
                .state
                .lock()
                .map_err(|_| HttpError::LocalUnavailable { timeout: false })?;
            state.used = now;
            if let Some(until) = state.cooldown.filter(|until| *until > now) {
                return Err(HttpError::RateLimited {
                    retry_after_seconds: Some(
                        until
                            .duration_since(now)
                            .as_secs()
                            .saturating_add(1)
                            .min(86400) as u32,
                    ),
                });
            }
        }
        Ok(HttpOperation {
            client: self,
            lane,
            _global: global,
            _provider: provider,
            deadline: now + OPERATION_TIMEOUT,
        })
    }
}
impl HttpOperation<'_> {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn rate_limit(&self, seconds: Option<u32>) -> HttpError {
        let seconds = seconds.unwrap_or(60).clamp(1, 86400);
        let Ok(mut state) = self.lane.state.lock() else {
            return HttpError::LocalUnavailable { timeout: false };
        };
        let until = Instant::now() + Duration::from_secs(u64::from(seconds));
        state.cooldown = Some(state.cooldown.map_or(until, |previous| previous.max(until)));
        HttpError::RateLimited {
            retry_after_seconds: Some(seconds),
        }
    }
    pub fn ensure_active(&self) -> Result<(), HttpError> {
        if Instant::now() >= self.deadline {
            Err(HttpError::Timeout)
        } else {
            Ok(())
        }
    }

    pub async fn get(
        &self,
        endpoint: &str,
        query: &[(String, String)],
    ) -> Result<Vec<u8>, HttpError> {
        let response = self
            .request(endpoint, query, &[], HttpRequestBody::Empty)
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(HttpError::Transport);
        }
        Ok(response.body)
    }

    pub async fn request(
        &self,
        endpoint: &str,
        query: &[(String, String)],
        headers: &[(String, String)],
        body: HttpRequestBody,
    ) -> Result<HttpResponse, HttpError> {
        if endpoint.trim() != endpoint
            || endpoint.contains('\\')
            || endpoint.chars().any(char::is_control)
        {
            return Err(HttpError::InvalidRequest);
        }
        let mut url = url::Url::parse(endpoint).map_err(|_| HttpError::InvalidRequest)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || query.len() > 64
        {
            return Err(HttpError::InvalidRequest);
        }
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        if url.as_str().len() > 16384 {
            return Err(HttpError::InvalidRequest);
        }
        let mut header_map = reqwest::header::HeaderMap::new();
        if headers.len() > 16 {
            return Err(HttpError::InvalidRequest);
        }
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpError::InvalidRequest)?;
            if !matches!(
                name.as_str(),
                "cookie" | "referer" | "origin" | "authorization"
            ) || header_map.contains_key(&name)
                || value.len() > 4096
            {
                return Err(HttpError::InvalidRequest);
            }
            if matches!(name.as_str(), "referer" | "origin") {
                let source = url::Url::parse(value).map_err(|_| HttpError::InvalidRequest)?;
                if source.origin() != url.origin()
                    || !source.username().is_empty()
                    || source.password().is_some()
                    || source.query().is_some()
                    || source.fragment().is_some()
                {
                    return Err(HttpError::InvalidRequest);
                }
            }
            let mut value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| HttpError::InvalidRequest)?;
            value.set_sensitive(true);
            header_map.insert(name, value);
        }
        let request = match body {
            HttpRequestBody::Empty => self.client.client.get(url),
            HttpRequestBody::Form(fields) => {
                validate_fields(&fields)?;
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(&fields)
                    .finish();
                if encoded.len() > MAX_FIELD_BYTES {
                    return Err(HttpError::InvalidRequest);
                }
                self.client
                    .client
                    .post(url)
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(encoded)
            }
            HttpRequestBody::Multipart {
                fields,
                file_name,
                field_name,
                file,
            } => {
                validate_fields(&fields)?;
                if file.len() > MAX_BODY_BYTES
                    || !safe_part_name(&file_name, 128)
                    || !safe_part_name(&field_name, 64)
                {
                    return Err(HttpError::InvalidRequest);
                }
                let part = reqwest::multipart::Part::bytes(file)
                    .file_name(file_name)
                    .mime_str("application/x-bittorrent")
                    .map_err(|_| HttpError::InvalidRequest)?;
                let mut form = reqwest::multipart::Form::new().part(field_name, part);
                for (name, value) in fields {
                    form = form.text(name, value);
                }
                self.client.client.post(url).multipart(form)
            }
        }
        .headers(header_map)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .build()
        .map_err(|_| HttpError::InvalidRequest)?;
        if request
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .is_some_and(|value| {
                value
                    .to_str()
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_none_or(|size| size > MAX_MULTIPART_BYTES)
            })
        {
            return Err(HttpError::InvalidRequest);
        }
        let mut dispatched = false;
        let work = async {
            let start = {
                let mut state = self
                    .lane
                    .state
                    .lock()
                    .map_err(|_| HttpError::LocalUnavailable { timeout: false })?;
                let start = state.next.max(Instant::now());
                state.next = start + MIN_REQUEST_INTERVAL;
                state.used = Instant::now();
                start
            };
            tokio::time::sleep_until(start).await;
            dispatched = true;
            let mut response = self
                .client
                .client
                .execute(request)
                .await
                .map_err(classify)?;
            let status = response.status();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(HttpError::Authentication);
            }
            if status.as_u16() == 429 {
                return Err(self.rate_limit(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(retry_after),
                ));
            }
            if status.is_redirection() {
                return Err(HttpError::Redirect);
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|v| v.as_bytes() != b"identity")
            {
                return Err(HttpError::InvalidResponse);
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_BODY_BYTES as u64)
            {
                return Err(HttpError::ResponseTooLarge);
            }
            let mut set_cookies = Vec::new();
            for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
                if set_cookies.len() >= 8 || value.as_bytes().len() > 4096 {
                    return Err(HttpError::InvalidResponse);
                }
                set_cookies.push(
                    value
                        .to_str()
                        .map_err(|_| HttpError::InvalidResponse)?
                        .to_owned(),
                );
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(classify)? {
                if chunk.len() > MAX_BODY_BYTES - bytes.len() {
                    return Err(HttpError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(HttpResponse {
                status: status.as_u16(),
                body: bytes,
                set_cookies,
            })
        };
        tokio::time::timeout_at(self.deadline - STATUS_RESERVE, work)
            .await
            .map_err(|_| {
                if dispatched {
                    HttpError::Timeout
                } else {
                    HttpError::LocalUnavailable { timeout: true }
                }
            })?
    }
}
fn safe_part_name(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | '"'))
}
fn validate_fields(fields: &[(String, String)]) -> Result<(), HttpError> {
    if fields.len() > 64
        || fields.iter().any(|(name, _)| !safe_part_name(name, 64))
        || fields
            .iter()
            .try_fold(0usize, |size, (name, value)| {
                size.checked_add(name.len())?.checked_add(value.len())
            })
            .is_none_or(|size| size > MAX_FIELD_BYTES)
    {
        return Err(HttpError::InvalidRequest);
    }
    Ok(())
}
fn classify(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Transport
    }
}

fn retry_after(value: &str) -> Option<u32> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.min(86400) as u32);
    }
    let date = chrono::DateTime::parse_from_rfc2822(value)
        .ok()?
        .timestamp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(
        date.saturating_sub(i64::try_from(now).ok()?)
            .clamp(1, 86400) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, response::IntoResponse, routing::get};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn private_post_payloads_status_and_cookie_bounds() {
        use axum::{
            body::Bytes,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        let app = Router::new()
            .route(
                "/form",
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(headers["cookie"], "SID=private");
                    assert_eq!(body, "password=a%26b%3D%2B&username=user");
                    (
                        StatusCode::CONFLICT,
                        [("set-cookie", "SID=secret; HttpOnly")],
                        "rejected",
                    )
                }),
            )
            .route(
                "/multipart",
                post(|headers: HeaderMap, body: Bytes| async move {
                    assert!(
                        headers["content-type"]
                            .to_str()
                            .unwrap()
                            .starts_with("multipart/form-data; boundary=")
                    );
                    let body = String::from_utf8(body.to_vec()).unwrap();
                    assert!(body.contains("name=\"torrents\"; filename=\"item.torrent\""));
                    assert!(body.contains("application/x-bittorrent"));
                    assert!(body.contains("TORRENT_BYTES"));
                    assert!(body.contains("name=\"category\""));
                    "accepted"
                }),
            )
            .route(
                "/cookies",
                get(|| async { ([("set-cookie", "s".repeat(4097))], "body") }),
            )
            .route(
                "/failure",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "private") }),
            );
        let app = app.route("/status/{code}", get(|axum::extract::Path(code): axum::extract::Path<u16>, headers: HeaderMap| async move {
            assert!(headers.get("cookie").is_none(), "No ambient cookie jar after a SID response");
            StatusCode::from_u16(code).unwrap()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = HttpClient::new().unwrap();
        let operation = client.operation(Uuid::new_v4()).unwrap();
        let response = operation
            .request(
                &format!("{base}/form"),
                &[],
                &[("Cookie".into(), "SID=private".into())],
                HttpRequestBody::Form(vec![
                    ("password".into(), "a&b=+".into()),
                    ("username".into(), "user".into()),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(response.set_cookies, ["SID=secret; HttpOnly"]);
        let response = operation
            .request(
                &format!("{base}/multipart"),
                &[],
                &[],
                HttpRequestBody::Multipart {
                    fields: vec![("category".into(), "tv".into())],
                    file_name: "item.torrent".into(),
                    field_name: "torrents".into(),
                    file: b"TORRENT_BYTES".to_vec(),
                },
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            operation
                .get(&format!("{base}/failure"), &[])
                .await
                .unwrap_err(),
            HttpError::Transport,
            "Indexer GET retains non-success classification while qbit can inspect rejection statuses"
        );
        assert!(matches!(
            operation
                .request(&format!("{base}/cookies"), &[], &[], HttpRequestBody::Empty)
                .await,
            Err(HttpError::InvalidResponse)
        ));
        for headers in [
            vec![("Host".into(), "other".into())],
            vec![("Origin".into(), "http://other.invalid".into())],
            vec![("Cookie".into(), "injected\r\nX-Header: value".into())],
        ] {
            assert!(matches!(
                operation
                    .request(
                        &format!("{base}/form"),
                        &[],
                        &headers,
                        HttpRequestBody::Empty
                    )
                    .await,
                Err(HttpError::InvalidRequest)
            ));
        }
        assert!(
            matches!(
                operation
                    .request(
                        &base,
                        &[],
                        &[],
                        HttpRequestBody::Form(vec![("p".into(), "&".repeat(MAX_FIELD_BYTES))])
                    )
                    .await,
                Err(HttpError::InvalidRequest)
            ),
            "Encoded form limit also covers percent expansion"
        );
        assert!(matches!(
            operation
                .request(
                    &base,
                    &[],
                    &[],
                    HttpRequestBody::Multipart {
                        fields: vec![],
                        file_name: "a.torrent".into(),
                        field_name: "torrents".into(),
                        file: vec![0; MAX_BODY_BYTES + 1]
                    }
                )
                .await,
            Err(HttpError::InvalidRequest)
        ));
        assert!(matches!(
            operation
                .request(
                    &base,
                    &[],
                    &[],
                    HttpRequestBody::Multipart {
                        fields: vec![("category".into(), "x".repeat(MAX_FIELD_BYTES + 1))],
                        file_name: "a.torrent".into(),
                        field_name: "torrents".into(),
                        file: vec![]
                    }
                )
                .await,
            Err(HttpError::InvalidRequest)
        ));
        for status in [400, 404] {
            let response = operation
                .request(
                    &format!("{base}/status/{status}"),
                    &[],
                    &[],
                    HttpRequestBody::Empty,
                )
                .await
                .unwrap();
            assert_eq!(response.status, status);
        }
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn bounds_cooldowns_and_cancelled_leases_preserve_transport_safety() {
        let app = Router::new()
            .route("/ok", get(|| async { "okay" }))
            .route(
                "/redirect",
                get(|| async { (axum::http::StatusCode::FOUND, [("location", "/ok")]) }),
            )
            .route(
                "/rate",
                get(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "900000")],
                        "PRIVATE_BODY_SENTINEL",
                    )
                }),
            )
            .route(
                "/encoded",
                get(|| async { ([("content-encoding", "gzip")], "not decompressed") }),
            )
            .route(
                "/large",
                get(|| async { "x".repeat(MAX_BODY_BYTES + 1).into_response() }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    "late"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Arc::new(HttpClient::new().unwrap());
        let id = Uuid::new_v4();
        let operation = client.operation(id).unwrap();
        assert!(matches!(client.operation(id), Err(HttpError::Busy)));
        assert_eq!(
            operation.get(&format!("{base}/ok"), &[]).await.unwrap(),
            b"okay"
        );
        assert_eq!(
            operation
                .get(&format!("{base}/redirect"), &[])
                .await
                .unwrap_err(),
            HttpError::Redirect,
            "Following redirects would turn this into /ok success"
        );
        assert_eq!(
            operation
                .get(&format!("{base}/encoded"), &[])
                .await
                .unwrap_err(),
            HttpError::InvalidResponse
        );
        assert_eq!(
            operation
                .get(&format!("{base}/large"), &[])
                .await
                .unwrap_err(),
            HttpError::ResponseTooLarge
        );
        drop(operation);
        let mut limited = client.operation(Uuid::new_v4()).unwrap();
        limited.deadline = Instant::now() + STATUS_RESERVE + Duration::from_millis(40);
        assert_eq!(
            limited.get(&format!("{base}/slow"), &[]).await.unwrap_err(),
            HttpError::Timeout
        );
        drop(limited);
        let (started, ready) = tokio::sync::oneshot::channel();
        let task_client = client.clone();
        let slow = format!("{base}/slow");
        let cancelled = tokio::spawn(async move {
            let operation = task_client.operation(id).unwrap();
            started.send(()).unwrap();
            operation.get(&slow, &[]).await
        });
        ready.await.unwrap();
        cancelled.abort();
        let _ = cancelled.await;
        let recovered = client.operation(id).unwrap();
        drop(recovered);
        let mut held = Vec::new();
        for _ in 0..MAX_OPERATIONS {
            held.push(client.operation(Uuid::new_v4()).unwrap());
        }
        assert!(matches!(
            client.operation(Uuid::new_v4()),
            Err(HttpError::Busy)
        ));
        held.clear();
        let rate_id = Uuid::new_v4();
        let rate = client.operation(rate_id).unwrap();
        assert_eq!(
            rate.get(&format!("{base}/rate"), &[]).await.unwrap_err(),
            HttpError::RateLimited {
                retry_after_seconds: Some(86400)
            }
        );
        drop(rate);
        assert!(matches!(
            client.operation(rate_id),
            Err(HttpError::RateLimited {
                retry_after_seconds: Some(86400)
            })
        ));
        assert_eq!(retry_after("999999999"), Some(86400));
        // Malformed syntax tests rejection without assuming a distant date's weekday.
        assert_eq!(retry_after("invalid date"), None);
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120;
        let date = chrono::DateTime::from_timestamp(future as i64, 0)
            .unwrap()
            .to_rfc2822();
        assert!((118..=121).contains(&retry_after(&date).unwrap()));
        server.abort();
        let _ = server.await;
        // Unknown-length chunked bodies must be bounded while streaming, independently of Content-Length.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let oversized = tokio::spawn(async move {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let send = async {
                let mut headers = Vec::new();
                let mut byte = [0u8; 1];
                while !headers.ends_with(b"\r\n\r\n") && headers.len() < 32768 {
                    stream.read_exact(&mut byte).await?;
                    headers.push(byte[0]);
                }
                stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await?;
                let chunk = vec![b'x'; 65536];
                for _ in 0..17 {
                    stream.write_all(b"10000\r\n").await?;
                    stream.write_all(&chunk).await?;
                    stream.write_all(b"\r\n").await?;
                }
                stream.write_all(b"0\r\n\r\n").await
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), send).await;
        });
        let operation = client.operation(Uuid::new_v4()).unwrap();
        assert_eq!(
            operation.get(&url, &[]).await.unwrap_err(),
            HttpError::ResponseTooLarge
        );
        oversized.await.unwrap();
    }
    #[tokio::test]
    async fn health_provenance_distinguishes_local_pacing_from_dispatched_timeout() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entered = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(tokio::sync::Notify::new());
        let notify = reached.clone();
        let seen = entered.clone();
        let app = axum::Router::new().route(
            "/slow",
            axum::routing::get(move || {
                let seen = seen.clone();
                let notify = notify.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    notify.notify_one();
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    "late"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = HttpClient::new().unwrap();
        let mut operation = client.operation(Uuid::new_v4()).unwrap();
        operation.deadline = Instant::now() + STATUS_RESERVE + Duration::from_millis(40);
        operation.lane.state.lock().unwrap().next = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            operation.get(&url, &[]).await.unwrap_err(),
            HttpError::LocalUnavailable { timeout: true }
        );
        assert_eq!(
            entered.load(Ordering::SeqCst),
            0,
            "local pacing never dispatched"
        );
        drop(operation);
        let mut operation = client.operation(Uuid::new_v4()).unwrap();
        operation.deadline = Instant::now() + STATUS_RESERVE + Duration::from_secs(1);
        {
            let request = operation.get(&url, &[]);
            tokio::pin!(request);
            tokio::select! {
                outcome = &mut request => panic!("request ended before handler-entry evidence: {outcome:?}"),
                started = tokio::time::timeout(Duration::from_secs(5), reached.notified()) => started.unwrap(),
            }
            assert_eq!(request.await.unwrap_err(), HttpError::Timeout);
        }
        assert_eq!(
            entered.load(Ordering::SeqCst),
            1,
            "network timeout follows actual dispatch"
        );
        // A prior successful/failed request cannot bless a later undispatched timeout.
        operation.deadline = Instant::now() + STATUS_RESERVE + Duration::from_millis(40);
        operation.lane.state.lock().unwrap().next = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            operation.get(&url, &[]).await.unwrap_err(),
            HttpError::LocalUnavailable { timeout: true }
        );
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
    }
}
