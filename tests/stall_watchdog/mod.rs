//! Diagnostics for tests whose HTTP client, API server, mock upstream and workers share one
//! single-threaded runtime, where any blocking call stalls every request deadline at once.
//!
//! Three independent signals separate the possible causes of a client timeout:
//! * a heartbeat task records executor scheduling lag (the runtime thread was not polling);
//! * an OS-thread watchdog, which does not depend on the runtime, records what the runtime thread
//!   was doing during a stall (scheduler state, kernel wait channel and syscall);
//! * a middleware times API handlers (handler/database time as opposed to client or transport).
//!
//! Output carries labels and counters only: no bodies, paths of media or credentials.
use axum::{extract::Request, middleware::Next, response::Response};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const BEAT_INTERVAL: Duration = Duration::from_millis(20);
const WATCH_INTERVAL: Duration = Duration::from_millis(100);
/// A runtime that has not polled the heartbeat for this long is reported as stalled.
const STALL_THRESHOLD_MS: u64 = 750;
/// Handlers and client calls slower than this are logged even when the test passes.
const SLOW_MS: u128 = 1000;
const MAX_RECORDS: usize = 16;

/// State is per runtime thread: tests in one binary run in parallel, each on its own
/// current-thread runtime, so a process-wide static would blend their stalls together.
struct Shared {
    epoch: Instant,
    last_beat_ms: AtomicU64,
    max_lag_ms: AtomicU64,
    next_id: AtomicU64,
    records: Mutex<Vec<String>>,
    in_flight: Mutex<Vec<(u64, String, Instant)>>,
}
impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
    fn record(&self, line: String) {
        eprintln!("{line}");
        if let Ok(mut records) = self.records.lock()
            && records.len() < MAX_RECORDS
        {
            records.push(line);
        }
    }
    fn in_flight(&self) -> Vec<String> {
        self.in_flight
            .lock()
            .map(|flight| {
                flight
                    .iter()
                    .map(|(_, label, since)| format!("{label} {}ms", since.elapsed().as_millis()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
thread_local! {
    static CURRENT: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}
fn current() -> Option<Arc<Shared>> {
    CURRENT.with(|current| current.borrow().clone())
}

/// Linux x86-64 syscall numbers that explain a stalled thread; anything else prints the number.
fn syscall_name(number: &str) -> String {
    match number {
        "7" => "poll".into(),
        "17" => "pread64".into(),
        "35" => "nanosleep".into(),
        "72" => "fcntl".into(),
        "74" => "fsync".into(),
        "75" => "fdatasync".into(),
        "202" => "futex".into(),
        "230" => "clock_nanosleep".into(),
        "232" => "epoll_wait".into(),
        other => format!("syscall_{other}"),
    }
}
fn thread_file(tid: &str, name: &str) -> String {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/{name}"))
        .unwrap_or_default()
        .trim()
        .to_owned()
}
/// Runtime thread ids come from `/proc/thread-self` -> `<pid>/task/<tid>`.
fn current_tid() -> Option<String> {
    let link = std::fs::read_link("/proc/thread-self").ok()?;
    Some(link.file_name()?.to_str()?.to_owned())
}

pub struct Watchdog {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    beat: tokio::task::JoinHandle<()>,
}
impl Watchdog {
    /// Call from the runtime thread under test.
    pub fn start() -> Self {
        let tid = current_tid().unwrap_or_default();
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            last_beat_ms: AtomicU64::new(0),
            max_lag_ms: AtomicU64::new(0),
            next_id: AtomicU64::new(0),
            records: Mutex::new(Vec::new()),
            in_flight: Mutex::new(Vec::new()),
        });
        CURRENT.with(|current| *current.borrow_mut() = Some(shared.clone()));
        let beating = shared.clone();
        let beat = tokio::spawn(async move {
            loop {
                let before = beating.now_ms();
                tokio::time::sleep(BEAT_INTERVAL).await;
                let after = beating.now_ms();
                beating.max_lag_ms.fetch_max(
                    (after - before).saturating_sub(BEAT_INTERVAL.as_millis() as u64),
                    Ordering::AcqRel,
                );
                beating.last_beat_ms.store(after, Ordering::Release);
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut reported = false;
            while !flag.load(Ordering::Acquire) {
                std::thread::sleep(WATCH_INTERVAL);
                let age = shared
                    .now_ms()
                    .saturating_sub(shared.last_beat_ms.load(Ordering::Acquire));
                if age >= STALL_THRESHOLD_MS && !reported {
                    reported = true;
                    let stat = thread_file(&tid, "stat");
                    // Field 3 of /proc/<pid>/task/<tid>/stat, after the parenthesised name.
                    let state = stat
                        .rsplit_once(')')
                        .and_then(|(_, rest)| rest.split_whitespace().next())
                        .unwrap_or("?");
                    let syscall = thread_file(&tid, "syscall");
                    shared.record(
                        serde_json::json!({"event":"executor_stall","stalled_ms":age,
                        "thread_state":state,"wchan":thread_file(&tid, "wchan"),
                        "syscall":syscall_name(syscall.split_whitespace().next().unwrap_or("?")),
                        "in_flight":shared.in_flight()})
                        .to_string(),
                    );
                } else if age < STALL_THRESHOLD_MS {
                    reported = false;
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
            beat,
        }
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.beat.abort();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        CURRENT.with(|current| *current.borrow_mut() = None);
    }
}

/// Axum middleware (`axum::middleware::from_fn(stall_watchdog::track)`) timing each API handler.
pub async fn track(request: Request, next: Next) -> Response {
    let Some(shared) = current() else {
        return next.run(request).await;
    };
    let label = format!("{} {}", request.method(), request.uri().path());
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    if let Ok(mut flight) = shared.in_flight.lock() {
        flight.push((id, label.clone(), started));
    }
    let response = next.run(request).await;
    if let Ok(mut flight) = shared.in_flight.lock() {
        flight.retain(|(other, _, _)| *other != id);
    }
    if started.elapsed().as_millis() >= SLOW_MS {
        shared.record(
            serde_json::json!({"event":"slow_handler","request":label,
            "ms":started.elapsed().as_millis()})
            .to_string(),
        );
    }
    response
}
/// Logs a client call that completed but took suspiciously long (near misses in passing runs).
pub fn note_client_call(label: &str, elapsed: Duration) {
    if let Some(shared) = current()
        && elapsed.as_millis() >= SLOW_MS
    {
        shared.record(
            serde_json::json!({"event":"slow_client_call","request":label,
            "ms":elapsed.as_millis()})
            .to_string(),
        );
    }
}
/// Summary for panic messages: executor lag, recorded stalls/slow calls and live handlers.
pub fn report() -> String {
    let Some(shared) = current() else {
        return "watchdog_inactive".into();
    };
    let records = shared
        .records
        .lock()
        .map(|r| r.join(" | "))
        .unwrap_or_default();
    format!(
        "max_executor_lag_ms={} in_flight=[{}] records=[{records}]",
        shared.max_lag_ms.load(Ordering::Acquire),
        shared.in_flight().join(", ")
    )
}
