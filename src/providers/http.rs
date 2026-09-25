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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    InvalidRequest,
    Authentication,
    RateLimited { retry_after_seconds: Option<u32> },
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
        let mut lanes = self.lanes.lock().map_err(|_| HttpError::Transport)?;
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
            let mut state = lane.state.lock().map_err(|_| HttpError::Transport)?;
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
            return HttpError::Transport;
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
        let work = async {
            let start = {
                let mut state = self.lane.state.lock().map_err(|_| HttpError::Transport)?;
                let start = state.next.max(Instant::now());
                state.next = start + MIN_REQUEST_INTERVAL;
                state.used = Instant::now();
                start
            };
            tokio::time::sleep_until(start).await;
            let mut response = self
                .client
                .client
                .get(url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send()
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
            if !status.is_success() {
                return Err(HttpError::Transport);
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
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(classify)? {
                if chunk.len() > MAX_BODY_BYTES - bytes.len() {
                    return Err(HttpError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        };
        tokio::time::timeout_at(self.deadline - STATUS_RESERVE, work)
            .await
            .map_err(|_| HttpError::Timeout)?
    }
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
}
