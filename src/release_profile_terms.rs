//! Bounded pure term matching for release restrictions; applicability belongs to the caller.
//!
//! Prepare/evaluate before opening a DB writer. Inside the writer, validate current
//! inputs and use cached_terms; a cache miss is never a negative restriction result.
//! Dialect limits are explicit errors. This adapter does not claim full .NET/culture
//! equivalence: duplicate/numbered captures, balancing groups, class subtraction,
//! ambiguous octal escapes, explicit Unicode properties and unsupported engine constructs
//! remain unsupported. Unicode case/class behavior follows the native engine and is
//! not evidence of deployment-specific .NET culture equivalence.
mod dialect;

use crate::api::MediaDomain;
use fancy_regex::{Expr, Regex, RegexBuilder};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

pub const MAX_PROFILES: usize = 1024;
pub const MAX_TERMS_PER_LIST: usize = 200;
pub const MAX_TERM_BYTES: usize = 2048;
pub const MAX_TOTAL_TERMS: usize = 4096;
pub const MAX_TOTAL_TERM_BYTES: usize = 1024 * 1024;
pub const MAX_TITLE_BYTES: usize = 4096;
const SEMANTIC_VERSION: u32 = 1;
const WORKER_SLOTS: usize = 2;
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const WORK_DEADLINE: Duration = Duration::from_secs(9);
const MAX_AST_DEPTH: usize = 64;
const MAX_REPEAT: usize = 4096;
const MAX_TERM_AST_WORK: usize = 262_144;
const MAX_CATALOG_AST_WORK: usize = 1_048_576;
const MAX_BACKTRACKS: usize = 100_000;
const DELEGATE_BYTES: usize = 65_536;
const CACHE_ENTRIES: usize = 128;
const CACHE_BYTES: usize = 8 * 1024 * 1024;

type Digest = [u8; 32];
pub type Result<T> = std::result::Result<T, TermError>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermError {
    InvalidSyntax,
    UnsupportedDialect,
    LimitExceeded,
    Busy,
    Timeout,
    WorkerFailed,
    StateChanged,
}
impl TermError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidSyntax => "release_term_invalid_syntax",
            Self::UnsupportedDialect => "release_term_unsupported_dialect",
            Self::LimitExceeded => "release_term_limit_exceeded",
            Self::Busy => "release_term_busy",
            Self::Timeout => "release_term_timeout",
            Self::WorkerFailed => "release_term_worker_failed",
            Self::StateChanged => "release_term_state_changed",
        }
    }
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Busy | Self::Timeout | Self::WorkerFailed | Self::StateChanged
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileTerms {
    pub required: Vec<String>,
    pub ignored: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileMatches {
    pub required: Vec<usize>,
    pub ignored: Vec<usize>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermMatches {
    pub profiles: Vec<ProfileMatches>,
}
#[derive(Clone)]
pub struct PreparedTerms {
    inner: Arc<Prepared>,
}
struct Prepared {
    digest: Digest,
    profiles: Vec<ProfileTerms>,
    compiled: Vec<CompiledProfile>,
}
struct CompiledProfile {
    required: Vec<Matcher>,
    ignored: Vec<Matcher>,
}
enum Matcher {
    Literal(String),
    Regex(Regex),
}
impl PreparedTerms {
    pub fn digest(&self) -> Digest {
        self.inner.digest
    }
    pub fn profiles(&self) -> &[ProfileTerms] {
        &self.inner.profiles
    }
}
// Debug must not expose private user terms or compiled patterns.
impl std::fmt::Debug for PreparedTerms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedTerms")
            .field("profiles", &self.inner.profiles.len())
            .finish_non_exhaustive()
    }
}
fn digest_terms(media: MediaDomain, profiles: &[ProfileTerms]) -> Result<Digest> {
    if profiles.len() > MAX_PROFILES {
        return Err(TermError::LimitExceeded);
    }
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"hrrdarr.release-terms\0");
    hash.update(&SEMANTIC_VERSION.to_le_bytes());
    hash.update(&[match media {
        MediaDomain::Tv => 0,
        MediaDomain::Movies => 1,
    }]);
    hash.update(&(profiles.len() as u64).to_le_bytes());
    let mut count = 0usize;
    let mut bytes = 0usize;
    for profile in profiles {
        for list in [&profile.required, &profile.ignored] {
            if list.len() > MAX_TERMS_PER_LIST {
                return Err(TermError::LimitExceeded);
            }
            hash.update(&(list.len() as u64).to_le_bytes());
            for term in list {
                count = count.checked_add(1).ok_or(TermError::LimitExceeded)?;
                bytes = bytes
                    .checked_add(term.len())
                    .ok_or(TermError::LimitExceeded)?;
                if term.len() > MAX_TERM_BYTES
                    || count > MAX_TOTAL_TERMS
                    || bytes > MAX_TOTAL_TERM_BYTES
                {
                    return Err(TermError::LimitExceeded);
                }
                if term.trim().is_empty() {
                    return Err(TermError::InvalidSyntax);
                }
                hash.update(&(term.len() as u64).to_le_bytes());
                hash.update(term.as_bytes());
            }
        }
    }
    Ok(hash
        .finish()
        .as_ref()
        .try_into()
        .map_err(|_| TermError::WorkerFailed)?)
}
fn title_digest(title: &str) -> Result<Digest> {
    if title.len() > MAX_TITLE_BYTES {
        return Err(TermError::LimitExceeded);
    }
    Ok(
        ring::digest::digest(&ring::digest::SHA256, title.as_bytes())
            .as_ref()
            .try_into()
            .map_err(|_| TermError::WorkerFailed)?,
    )
}
static CPU: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
async fn bounded<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    bounded_with_timeout(work, CALL_TIMEOUT).await
}
async fn bounded_with_timeout<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
    timeout: Duration,
) -> Result<T> {
    let permit = CPU
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(WORKER_SLOTS)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| TermError::Busy)?;
    // spawn_blocking cannot be stopped once running. If the caller is cancelled or
    // its deadline expires, at most WORKER_SLOTS bounded closures may finish in
    // the background. Each retains its permit until actual completion; no cache
    // mutation is performed by a detached closure and no extra worker is admitted.
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::time::timeout(timeout, task)
        .await
        .map_err(|_| TermError::Timeout)?
        .map_err(|_| TermError::WorkerFailed)?
}
fn deadline(started: Instant) -> Result<()> {
    if started.elapsed() >= WORK_DEADLINE {
        Err(TermError::Timeout)
    } else {
        Ok(())
    }
}
fn lower_literal(value: &str) -> String {
    // Per-scalar simple lowering avoids context-sensitive final sigma and expanding
    // normalization. No title-wide Unicode normalization or regex case option is applied.
    value
        .chars()
        .map(|c| {
            let mut lowered = c.to_lowercase();
            let first = lowered.next().unwrap_or(c);
            if lowered.next().is_none() { first } else { c }
        })
        .collect()
}
fn compile(pattern: &str, total_work: &mut usize) -> Result<Regex> {
    let tree = Expr::parse_tree(pattern).map_err(engine_error)?;
    let mut stack = vec![(&tree.expr, 1usize, 0usize)];
    let mut term_work = 0usize;
    while let Some((expr, multiplier, depth)) = stack.pop() {
        term_work = term_work
            .checked_add(multiplier)
            .ok_or(TermError::LimitExceeded)?;
        *total_work = total_work
            .checked_add(multiplier)
            .ok_or(TermError::LimitExceeded)?;
        if depth > MAX_AST_DEPTH
            || term_work > MAX_TERM_AST_WORK
            || *total_work > MAX_CATALOG_AST_WORK
        {
            return Err(TermError::LimitExceeded);
        }
        let factor = match expr {
            Expr::Repeat { lo, hi, .. } => {
                if *lo > MAX_REPEAT || (*hi != usize::MAX && *hi > MAX_REPEAT) {
                    return Err(TermError::LimitExceeded);
                }
                if *hi == usize::MAX {
                    MAX_TITLE_BYTES + 1
                } else {
                    *hi
                }
            }
            Expr::Absent(_)
            | Expr::SubroutineCall(_)
            | Expr::BackrefWithRelativeRecursionLevel { .. } => {
                return Err(TermError::UnsupportedDialect);
            }
            _ => 1,
        };
        for child in expr.children_iter() {
            stack.push((
                child,
                multiplier
                    .checked_mul(factor)
                    .ok_or(TermError::LimitExceeded)?,
                depth + 1,
            ));
        }
    }
    RegexBuilder::new(pattern)
        .backtrack_limit(MAX_BACKTRACKS)
        .delegate_size_limit(DELEGATE_BYTES)
        .delegate_dfa_size_limit(DELEGATE_BYTES)
        .build()
        .map_err(engine_error)
}
fn engine_error(error: fancy_regex::Error) -> TermError {
    match error {
        fancy_regex::Error::RuntimeError(_) => TermError::Timeout,
        fancy_regex::Error::ParseError(_, fancy_regex::ParseError::RecursionExceeded) => {
            TermError::LimitExceeded
        }
        fancy_regex::Error::CompileError(error) => match *error {
            fancy_regex::CompileError::InnerError(error) if error.size_limit().is_some() => {
                TermError::LimitExceeded
            }
            fancy_regex::CompileError::LookBehindNotConst
            | fancy_regex::CompileError::VariableLookBehindRequiresFeature
            | fancy_regex::CompileError::FeatureNotYetSupported(_) => TermError::UnsupportedDialect,
            _ => TermError::InvalidSyntax,
        },
        _ => TermError::InvalidSyntax,
    }
}
fn prepare(media: MediaDomain, profiles: Vec<ProfileTerms>) -> Result<PreparedTerms> {
    let digest = digest_terms(media, &profiles)?;
    let started = Instant::now();
    let mut total_work = 0;
    let mut list = |terms: &[String]| -> Result<Vec<Matcher>> {
        terms
            .iter()
            .map(|term| {
                deadline(started)?;
                match dialect::recognize(term) {
                    dialect::Term::Literal(value) => Ok(Matcher::Literal(lower_literal(value))),
                    dialect::Term::Regex { pattern, flags } => {
                        let normalized = dialect::normalize(pattern, flags)?;
                        compile(&normalized, &mut total_work).map(Matcher::Regex)
                    }
                }
            })
            .collect()
    };
    let compiled = profiles
        .iter()
        .map(|p| {
            Ok(CompiledProfile {
                required: list(&p.required)?,
                ignored: list(&p.ignored)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    deadline(started)?;
    Ok(PreparedTerms {
        inner: Arc::new(Prepared {
            digest,
            profiles,
            compiled,
        }),
    })
}
pub async fn prepare_terms(
    media: MediaDomain,
    profiles: Vec<ProfileTerms>,
) -> Result<PreparedTerms> {
    digest_terms(media, &profiles)?;
    bounded(move || prepare(media, profiles)).await
}
pub fn validate_prepared(
    prepared: &PreparedTerms,
    media: MediaDomain,
    current: &[ProfileTerms],
) -> Result<()> {
    if digest_terms(media, current)? == prepared.digest() {
        Ok(())
    } else {
        Err(TermError::StateChanged)
    }
}
fn evaluate(prepared: &PreparedTerms, title: &str) -> Result<TermMatches> {
    title_digest(title)?;
    let started = Instant::now();
    let literal = lower_literal(title);
    let list = |matchers: &[Matcher]| -> Result<Vec<usize>> {
        let mut matches = Vec::new();
        for (i, matcher) in matchers.iter().enumerate() {
            deadline(started)?;
            let matched = match matcher {
                Matcher::Literal(term) => literal.contains(term),
                Matcher::Regex(regex) => regex.is_match(title).map_err(engine_error)?,
            };
            if matched {
                matches.push(i);
            }
        }
        Ok(matches)
    };
    let profiles = prepared
        .inner
        .compiled
        .iter()
        .map(|p| {
            Ok(ProfileMatches {
                required: list(&p.required)?,
                ignored: list(&p.ignored)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    deadline(started)?;
    Ok(TermMatches { profiles })
}
struct Cached {
    terms: Digest,
    title: Digest,
    matches: TermMatches,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Cached>,
    bytes: usize,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn read_cache(terms: Digest, title: Digest) -> Result<TermMatches> {
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .map_err(|_| TermError::WorkerFailed)?
        .entries
        .iter()
        .find(|entry| entry.terms == terms && entry.title == title)
        .map(|entry| entry.matches.clone())
        .ok_or(TermError::StateChanged)
}
fn write_cache(terms: Digest, title: Digest, matches: TermMatches) -> Result<()> {
    let bytes = std::mem::size_of::<Cached>()
        + matches.profiles.capacity() * std::mem::size_of::<ProfileMatches>()
        + matches
            .profiles
            .iter()
            .map(|p| (p.required.capacity() + p.ignored.capacity()) * std::mem::size_of::<usize>())
            .sum::<usize>();
    if bytes > CACHE_BYTES {
        return Err(TermError::LimitExceeded);
    }
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .map_err(|_| TermError::WorkerFailed)?;
    if let Some(index) = cache
        .entries
        .iter()
        .position(|e| e.terms == terms && e.title == title)
    {
        let old = cache.entries.remove(index).ok_or(TermError::WorkerFailed)?;
        cache.bytes -= old.bytes;
    }
    while cache.entries.len() >= CACHE_ENTRIES || cache.bytes + bytes > CACHE_BYTES {
        let old = cache.entries.pop_front().ok_or(TermError::WorkerFailed)?;
        cache.bytes -= old.bytes;
    }
    cache.bytes += bytes;
    cache.entries.push_back(Cached {
        terms,
        title,
        matches,
        bytes,
    });
    Ok(())
}
pub async fn evaluate_terms(prepared: &PreparedTerms, title: &str) -> Result<TermMatches> {
    let key = title_digest(title)?;
    match read_cache(prepared.digest(), key) {
        Ok(matches) => return Ok(matches),
        Err(TermError::StateChanged) => {}
        Err(error) => return Err(error),
    }
    let owned = prepared.clone();
    let title = title.to_owned();
    let matches = bounded(move || evaluate(&owned, &title)).await?;
    // Publish complete results only after successful worker join. Unknown/partial
    // results are never cached, including when a caller cancels the await.
    write_cache(prepared.digest(), key, matches.clone())?;
    Ok(matches)
}
pub fn cached_terms(
    media: MediaDomain,
    current: &[ProfileTerms],
    title: &str,
) -> Result<TermMatches> {
    read_cache(digest_terms(media, current)?, title_digest(title)?)
}

#[cfg(test)]
#[path = "release_profile_terms/tests.rs"]
mod tests;
