use super::*;
use crate::db::custom_formats::{Condition, FormatScore};
use std::sync::OnceLock;
/// Measured or parsed facts, not cached scores. Missing language differs from a measured empty list.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub version: u8,
    pub title: String,
    pub filename: Option<String>,
    pub languages: Option<Vec<i32>>,
    pub release_group: Option<String>,
    pub indexer_flags: Option<i64>,
    pub release_type: Option<i32>,
    pub size: Option<u64>,
    pub year: Option<i64>,
    pub edition: Option<String>,
    pub source: i32,
    pub resolution: i32,
    pub modifier: i32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, ts_rs::TS)]
pub struct Score {
    pub format_ids: Vec<i64>,
    pub score: i64,
}
const MATCH_WORKER_SLOTS: usize = 2;
const MATCH_CALL_TIMEOUT_SECONDS: u64 = 10;
const MATCH_COOPERATIVE_DEADLINE_SECONDS: u64 = 9;
const MAX_REGEX_AST_DEPTH: usize = 64;
const MAX_REGEX_AST_WORK: usize = 262_144;
const MAX_REGEX_REPEAT_COUNT: usize = 4096;
const MAX_FACT_TEXT_BYTES: usize = 4096;
const MAX_FACT_LABEL_BYTES: usize = 1024;
const MAX_LANGUAGE_FACTS: usize = 64;
const MAX_REGEX_BACKTRACKS: usize = 100_000;
const REGEX_DELEGATE_BYTES: usize = 65_536;
const MAX_CACHED_SCORES: usize = 2048;
static CPU: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
async fn bounded<T: Send + 'static>(
    work: impl FnOnce() -> std::result::Result<T, &'static str> + Send + 'static,
) -> std::result::Result<T, &'static str> {
    let permit = CPU
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MATCH_WORKER_SLOTS)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "custom_format_busy")?;
    // Cancellation cannot release admission while the blocking worker is still running.
    tokio::time::timeout(
        std::time::Duration::from_secs(MATCH_CALL_TIMEOUT_SECONDS),
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        }),
    )
    .await
    .map_err(|_| "custom_format_timeout")?
    .map_err(|_| "custom_format_worker_failed")?
}
fn regex(pattern: &str) -> std::result::Result<fancy_regex::Regex, &'static str> {
    let tree = fancy_regex::Expr::parse_tree(pattern).map_err(|_| "custom_format_regex_invalid")?;
    let mut work = vec![(&tree.expr, 1usize, 0usize)];
    let mut total = 0usize;
    while let Some((expr, multiplier, depth)) = work.pop() {
        if depth > MAX_REGEX_AST_DEPTH {
            return Err("custom_format_regex_complexity");
        }
        total = total
            .checked_add(multiplier)
            .ok_or("custom_format_regex_complexity")?;
        if total > MAX_REGEX_AST_WORK {
            return Err("custom_format_regex_complexity");
        }
        let factor = match expr {
            fancy_regex::Expr::Repeat { lo, hi, .. } => {
                if *lo > MAX_REGEX_REPEAT_COUNT
                    || (*hi != usize::MAX && *hi > MAX_REGEX_REPEAT_COUNT)
                {
                    return Err("custom_format_regex_complexity");
                };
                if *hi == usize::MAX {
                    MAX_FACT_TEXT_BYTES + 1
                } else {
                    *hi
                }
            }
            fancy_regex::Expr::Absent(_)
            | fancy_regex::Expr::SubroutineCall(_)
            | fancy_regex::Expr::BackrefWithRelativeRecursionLevel { .. } => {
                return Err("custom_format_regex_complexity");
            }
            _ => 1,
        };
        for child in expr.children_iter() {
            work.push((
                child,
                multiplier
                    .checked_mul(factor)
                    .ok_or("custom_format_regex_complexity")?,
                depth + 1,
            ));
        }
    }
    fancy_regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .backtrack_limit(MAX_REGEX_BACKTRACKS)
        .delegate_size_limit(REGEX_DELEGATE_BYTES)
        .delegate_dfa_size_limit(REGEX_DELEGATE_BYTES)
        .build()
        .map_err(|_| "custom_format_regex_invalid")
}
fn pattern(c: &Condition) -> Option<&str> {
    match c {
        Condition::ReleaseTitle { pattern }
        | Condition::ReleaseGroup { pattern }
        | Condition::Edition { pattern } => Some(pattern),
        _ => None,
    }
}
pub(crate) async fn compile_specs(
    specs: Vec<Specification>,
) -> std::result::Result<(), &'static str> {
    bounded(move || {
        let started = std::time::Instant::now();
        for s in specs {
            if started.elapsed()
                > std::time::Duration::from_secs(MATCH_COOPERATIVE_DEADLINE_SECONDS)
            {
                return Err("custom_format_timeout");
            }
            if let Some(p) = pattern(&s.condition) {
                regex(p)?;
            }
        }
        Ok(())
    })
    .await
}
// ponytail: process-wide 2048-entry cache may evict warmed decisions and explicitly
// reject state_changed; use per-operation evaluated snapshots if measured contention warrants it.
static SCORES: OnceLock<std::sync::Mutex<std::collections::VecDeque<([u8; 32], Score)>>> =
    OnceLock::new();
pub(crate) async fn score(
    formats: &[Format],
    media: MediaDomain,
    evidence: Evidence,
    original_language: Option<i64>,
    scores: Vec<FormatScore>,
    cached_only: bool,
) -> crate::search::Result<Score> {
    validate_evidence(media, &evidence).map_err(crate::search::SearchError)?;
    if formats.is_empty() {
        return Ok(Score::default());
    }
    let bytes = serde_json::to_vec(&(media, formats, &evidence, original_language, &scores))
        .map_err(|_| crate::search::SearchError("custom_format_facts_invalid"))?;
    let key: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .try_into()
        .map_err(|_| crate::search::SearchError("custom_format_facts_invalid"))?;
    let cache = SCORES.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()));
    if let Some(value) = cache
        .lock()
        .map_err(|_| crate::search::SearchError("custom_format_cache_failed"))?
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
    {
        return Ok(value);
    }
    if cached_only {
        return Err(crate::search::SearchError("custom_format_state_changed"));
    }
    let result = evaluate(formats.to_vec(), media, evidence, original_language, scores)
        .await
        .map_err(crate::search::SearchError)?;
    let mut cache = cache
        .lock()
        .map_err(|_| crate::search::SearchError("custom_format_cache_failed"))?;
    if cache.len() >= MAX_CACHED_SCORES {
        cache.pop_front();
    }
    cache.push_back((key, result.clone()));
    Ok(result)
}
pub(crate) fn validate_evidence(
    media: MediaDomain,
    evidence: &Evidence,
) -> std::result::Result<(), &'static str> {
    if evidence.version != 1
        || evidence.title.is_empty()
        || evidence.title.chars().any(char::is_control)
        || evidence.indexer_flags.is_some_and(|v| v < 0)
        || evidence.release_type.is_some_and(|v| !(0..=3).contains(&v))
        || !(0..=if matches!(media, MediaDomain::Tv) {
            7
        } else {
            9
        })
            .contains(&evidence.source)
        || ![0, 360, 480, 540, 576, 720, 1080, 2160].contains(&evidence.resolution)
        || !(0..=5).contains(&evidence.modifier)
        || evidence.title.len() > MAX_FACT_TEXT_BYTES
        || evidence
            .filename
            .as_ref()
            .is_some_and(|s| s.len() > MAX_FACT_TEXT_BYTES)
        || evidence
            .release_group
            .as_ref()
            .is_some_and(|s| s.len() > MAX_FACT_LABEL_BYTES)
        || evidence
            .edition
            .as_ref()
            .is_some_and(|s| s.len() > MAX_FACT_LABEL_BYTES)
        || evidence.languages.as_ref().is_some_and(|l| {
            l.len() > MAX_LANGUAGE_FACTS
                || l.iter().any(|id| {
                    !(0..=if matches!(media, MediaDomain::Tv) {
                        52
                    } else {
                        57
                    })
                        .contains(id)
                })
        })
    {
        return Err("custom_format_facts_invalid");
    }
    Ok(())
}
pub(crate) async fn evaluate(
    formats: Vec<Format>,
    media: MediaDomain,
    evidence: Evidence,
    original_language: Option<i64>,
    scores: Vec<FormatScore>,
) -> std::result::Result<Score, &'static str> {
    validate_evidence(media, &evidence)?;
    bounded(move || {
        let started = std::time::Instant::now();
        let mut result = Score::default();
        for format in formats {
            let mut groups = std::collections::BTreeMap::<u8, (bool, bool)>::new();
            for spec in &format.definition.specifications {
                if started.elapsed()
                    > std::time::Duration::from_secs(MATCH_COOPERATIVE_DEADLINE_SECONDS)
                {
                    return Err("custom_format_timeout");
                }
                let matched = matches_spec(spec, media, &evidence, original_language)?;
                let group = groups.entry(kind(&spec.condition)).or_insert((true, false));
                group.0 &= !spec.required || matched;
                group.1 |= matched;
            }
            if !groups.is_empty() && groups.values().all(|(required, any)| *required && *any) {
                result.format_ids.push(format.id);
                let value = scores
                    .iter()
                    .find(|s| s.format_id == format.id)
                    .map_or(0, |s| i64::from(s.score));
                result.score = result
                    .score
                    .checked_add(value)
                    .ok_or("custom_format_score_overflow")?;
            }
        }
        Ok(result)
    })
    .await
}
fn kind(c: &Condition) -> u8 {
    match c {
        Condition::ReleaseTitle { .. } => 0,
        Condition::ReleaseGroup { .. } => 1,
        Condition::Edition { .. } => 2,
        Condition::Language { .. } => 3,
        Condition::Size { .. } => 4,
        Condition::Source { .. } => 5,
        Condition::Resolution { .. } => 6,
        Condition::QualityModifier { .. } => 7,
        Condition::IndexerFlag { .. } => 8,
        Condition::ReleaseType { .. } => 9,
        Condition::Year { .. } => 10,
    }
}
fn matches_spec(
    s: &Specification,
    media: MediaDomain,
    e: &Evidence,
    original: Option<i64>,
) -> std::result::Result<bool, &'static str> {
    let match_text =
        |pattern: &str, text: Option<&str>| -> std::result::Result<bool, &'static str> {
            match text {
                None => Ok(false),
                Some(t) => regex(pattern)?
                    .is_match(t)
                    .map_err(|_| "custom_format_regex_failed"),
            }
        };
    let matched = match &s.condition {
        Condition::ReleaseTitle { pattern } => {
            let title = if matches!(media, MediaDomain::Movies) {
                simplified_title(&e.title, e.year)
            } else {
                e.title.clone()
            };
            match_text(pattern, Some(&title))? || match_text(pattern, e.filename.as_deref())?
        }
        Condition::ReleaseGroup { pattern } => match_text(pattern, e.release_group.as_deref())?,
        Condition::Edition { pattern } => match_text(pattern, e.edition.as_deref())?,
        Condition::Language {
            value,
            except_language,
        } => {
            let Some(languages) = &e.languages else {
                return Ok(false);
            };
            let compared = if *value == -2 {
                original
                    .filter(|v| *v > 0)
                    .and_then(|v| i32::try_from(v).ok())
                    .unwrap_or(-2)
            } else {
                *value
            };
            if *except_language {
                languages.iter().any(|l| *l != compared)
            } else {
                languages.contains(&compared)
            }
        }
        Condition::Size { min_gib, max_gib } => e.size.is_some_and(|v| {
            let gib = v as f64 / 1073741824.0;
            gib > *min_gib && gib <= *max_gib
        }),
        Condition::Source { value } => e.source == *value,
        Condition::Resolution { value } => e.resolution == *value,
        Condition::QualityModifier { value } => e.modifier == *value,
        Condition::IndexerFlag { value } => e
            .indexer_flags
            .is_some_and(|flags| flags & i64::from(*value) == i64::from(*value)),
        Condition::ReleaseType { value } => e.release_type == Some(*value),
        Condition::Year { min, max } => e
            .year
            .is_some_and(|v| v >= i64::from(*min) && v <= i64::from(*max)),
    };
    Ok(matched != s.negate)
}
// Movie title conditions target technical release text, without the movie title/year prefix.
fn simplified_title(title: &str, year: Option<i64>) -> String {
    let Some(year) = year else {
        return title.to_owned();
    };
    let Some(parsed) = crate::search::parser::parse(title, false).ok() else {
        return title.to_owned();
    };
    let Some(year_start) =
        title[..parsed.technical_start.unwrap_or(title.len())].rfind(&year.to_string())
    else {
        return title.to_owned();
    };
    let end = title[..year_start]
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '(' | '['))
        .len();
    let replacement = if title[..end].contains('.') {
        "A.Movie"
    } else {
        "A Movie"
    };
    format!("{replacement}{}", &title[end..])
}
fn structured_quality(
    source: &str,
    resolution: i64,
    modifier: Option<&str>,
    tv: bool,
) -> crate::search::Result<(i32, i32, i32)> {
    let sources: &[&str] = if tv {
        &[
            "unknown",
            "television",
            "televisionRaw",
            "web",
            "webRip",
            "dvd",
            "bluray",
            "blurayRaw",
        ]
    } else {
        &[
            "unknown",
            "cam",
            "telesync",
            "telecine",
            "workprint",
            "dvd",
            "tv",
            "webdl",
            "webrip",
            "bluray",
        ]
    };
    let source = sources
        .iter()
        .position(|v| v.eq_ignore_ascii_case(source))
        .ok_or(crate::search::SearchError("quality_facts_invalid"))? as i32;
    let modifier = ["none", "regional", "screener", "rawhd", "brdisk", "remux"]
        .iter()
        .position(|v| v.eq_ignore_ascii_case(modifier.unwrap_or("none")))
        .ok_or(crate::search::SearchError("quality_facts_invalid"))? as i32;
    Ok((
        source,
        i32::try_from(resolution)
            .map_err(|_| crate::search::SearchError("quality_facts_invalid"))?,
        modifier,
    ))
}
pub(crate) async fn populate_quality(
    c: &Connection,
    evidence: &mut Evidence,
    name: Option<&str>,
    tv: bool,
) -> crate::search::Result<()> {
    let Some(name) = name else { return Ok(()) };
    let Some(row)=c.query("SELECT source,resolution,modifier FROM quality_definitions WHERE media_type=? AND name=?",params![if tv{"tv"}else{"movies"},name]).await?.next().await? else{return Ok(())};
    (evidence.source, evidence.resolution, evidence.modifier) = structured_quality(
        &row.get::<String>(0)?,
        row.get(1)?,
        row.get::<Option<String>>(2)?.as_deref(),
        tv,
    )?;
    Ok(())
}
pub(crate) fn parsed(
    title: &str,
    parsed: &crate::search::parser::ParsedRelease,
    tv: bool,
    size: Option<u64>,
) -> Evidence {
    use crate::search::parser::Numbering;
    let (source, resolution, modifier) = (0, 0, 0);
    let tail = title.rsplit_once('-').map(|(_, s)| s).filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars().all(|c| c.is_ascii_alphanumeric())
            && !matches!(s.to_ascii_lowercase().as_str(), "dl" | "ray")
            && !s.chars().all(|c| c.is_ascii_digit())
    });
    let tokens = title
        .get(parsed.technical_start.unwrap_or(title.len())..)
        .unwrap_or("")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let mut languages = Vec::new();
    for token in tokens {
        if token.len() > 2 {
            if let Some(id) = language(token, tv) {
                if !languages.contains(&id) {
                    languages.push(id);
                }
            }
        }
    }
    Evidence {
        version: 1,
        title: title.into(),
        filename: None,
        languages: (!languages.is_empty()).then_some(languages),
        release_group: tail.map(str::to_owned),
        indexer_flags: None,
        release_type: if tv {
            Some(match &parsed.numbering {
                Some(Numbering::Episodes { episodes, .. }) if episodes.len() > 1 => 2,
                Some(Numbering::Season { .. }) => 3,
                Some(_) => 1,
                None => 0,
            })
        } else {
            None
        },
        size,
        year: parsed.year,
        edition: parsed.edition.clone(),
        source,
        resolution,
        modifier,
    }
}
pub(crate) fn language(text: &str, tv: bool) -> Option<i32> {
    let media = if tv {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    crate::languages::language_id(media, text)
        .or_else(|| crate::languages::iso_language_id(media, text))
        .filter(|id| *id >= 0)
}

pub(crate) fn release(
    release: &crate::providers::indexer::Release,
    parsed: &crate::search::parser::ParsedRelease,
    tv: bool,
) -> Evidence {
    let mut e = parsed_facts(
        release.metadata.title.as_deref().unwrap_or(""),
        parsed,
        tv,
        release.metadata.size_bytes,
    );
    if !release.metadata.languages.is_empty() {
        let values: Option<Vec<_>> = release
            .metadata
            .languages
            .iter()
            .map(|s| language(s, tv))
            .collect();
        e.languages = values;
    }
    let f = &release.facts;
    let mut flags = 0i64;
    if let Some(t) = &f.torrent {
        flags |= match t.download_volume_factor {
            Some(0.0) => 1,
            Some(0.5) => 2,
            Some(0.75) => {
                if tv {
                    32
                } else {
                    256
                }
            }
            Some(0.25) => {
                if tv {
                    64
                } else {
                    512
                }
            }
            _ => 0,
        };
        if t.upload_volume_factor == Some(2.0) {
            flags |= 4
        }
        if t.internal == Some(true) {
            flags |= if tv { 8 } else { 32 }
        }
    }
    if f.scene == Some(true) {
        flags |= if tv { 16 } else { 128 }
    }
    if f.nuked == Some(true) {
        flags |= if tv { 128 } else { 2048 }
    }
    if tv && f.has_subtitles == Some(true) {
        flags |= 256
    }
    e.indexer_flags = Some(flags);
    e
}
fn parsed_facts(
    title: &str,
    release: &crate::search::parser::ParsedRelease,
    tv: bool,
    size: Option<u64>,
) -> Evidence {
    parsed(title, release, tv, size)
}
pub(crate) async fn existing(
    c: &Connection,
    media: MediaDomain,
    file: i64,
) -> crate::search::Result<Evidence> {
    let tv = matches!(media, MediaDomain::Tv);
    let (table, column, edition) = if tv {
        ("episode_files", "episode_file_id", "NULL")
    } else {
        ("movie_files", "movie_file_id", "f.edition")
    };
    let row=c.query(&format!("SELECT f.path,m.original_release_title,m.original_file_path,m.languages_json,m.release_group,m.indexer_flags,m.release_type,m.size,q.source,{edition},q.resolution,q.modifier FROM {table} f LEFT JOIN file_metadata m ON m.{column}=f.id LEFT JOIN quality_definitions q ON q.media_type=m.media_type AND q.quality_id=m.quality_id WHERE f.id=?"),[file]).await?.next().await?.ok_or(crate::search::SearchError("existing_file_missing"))?;
    let path: String = row.get(0)?;
    let basename = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_owned();
    let original_path: Option<String> = row.get(2)?;
    let title = row
        .get::<Option<String>>(1)?
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            original_path
                .map(|s| basename(&s))
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| basename(&path));
    let parsed = crate::search::parser::parse(&title, tv).ok();
    let (source, resolution, modifier) = structured_quality(
        row.get::<Option<String>>(8)?
            .as_deref()
            .unwrap_or("unknown"),
        row.get::<Option<i64>>(10)?.unwrap_or(0),
        row.get::<Option<String>>(11)?.as_deref(),
        tv,
    )?;
    Ok(Evidence {
        version: 1,
        title,
        filename: Some(basename(&path)),
        languages: row
            .get::<Option<String>>(3)?
            .map(|s| {
                serde_json::from_str(&s)
                    .map_err(|_| crate::search::SearchError("existing_languages_invalid"))
            })
            .transpose()?,
        release_group: row.get(4)?,
        indexer_flags: row.get(5)?,
        release_type: row.get(6)?,
        size: row
            .get::<Option<i64>>(7)?
            .and_then(|v| u64::try_from(v).ok()),
        year: parsed.as_ref().and_then(|p| p.year),
        edition: row.get(9)?,
        source,
        resolution,
        modifier,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn evidence() -> Evidence {
        Evidence {
            version: 1,
            title: "Harbor.2020.English.1080p.WEB-DL-GROUP".into(),
            filename: None,
            languages: Some(vec![1, 2]),
            release_group: Some("GROUP".into()),
            indexer_flags: Some(9),
            release_type: Some(2),
            size: Some(2 * 1073741824),
            year: Some(2020),
            edition: Some("Extended".into()),
            source: 7,
            resolution: 1080,
            modifier: 5,
        }
    }
    fn spec(condition: Condition, required: bool, negate: bool) -> Specification {
        Specification {
            name: "condition".into(),
            required,
            negate,
            condition,
        }
    }
    #[tokio::test]
    async fn all_conditions_groups_language_and_regex_bounds() {
        let e = evidence();
        let conditions = vec![
            Condition::ReleaseTitle {
                pattern: r"(?<=2020\.)English".into(),
            },
            Condition::ReleaseGroup {
                pattern: "^group$".into(),
            },
            Condition::Edition {
                pattern: "extended".into(),
            },
            Condition::Language {
                value: -2,
                except_language: false,
            },
            Condition::Language {
                value: 1,
                except_language: true,
            },
            Condition::Size {
                min_gib: 1.0,
                max_gib: 2.0,
            },
            Condition::Source { value: 7 },
            Condition::Resolution { value: 1080 },
            Condition::QualityModifier { value: 5 },
            Condition::IndexerFlag { value: 8 },
            Condition::ReleaseType { value: 2 },
            Condition::Year {
                min: 2020,
                max: 2020,
            },
        ];
        for c in conditions {
            let s = spec(c, false, false);
            assert!(
                matches_spec(&s, MediaDomain::Movies, &e, Some(1)).unwrap(),
                "{s:?}"
            );
            let mut neg = s;
            neg.negate = true;
            assert!(!matches_spec(&neg, MediaDomain::Movies, &e, Some(1)).unwrap());
        }
        let mut empty = e.clone();
        empty.languages = None;
        for negate in [false, true] {
            for except in [false, true] {
                let s = spec(
                    Condition::Language {
                        value: 1,
                        except_language: except,
                    },
                    false,
                    negate,
                );
                assert!(!matches_spec(&s, MediaDomain::Tv, &empty, Some(1)).unwrap());
                empty.languages = Some(vec![]);
                assert_eq!(
                    matches_spec(&s, MediaDomain::Tv, &empty, Some(1)).unwrap(),
                    negate
                );
                empty.languages = None;
            }
        }
        assert!(
            !matches_spec(
                &spec(
                    Condition::Size {
                        min_gib: 2.0,
                        max_gib: 3.0
                    },
                    false,
                    false
                ),
                MediaDomain::Movies,
                &e,
                None
            )
            .unwrap()
        );
        let format = |specifications| Format {
            id: 1,
            media_type: MediaDomain::Movies,
            definition: Input {
                name: "test".into(),
                include_when_renaming: false,
                specifications,
            },
        };
        let yes = spec(
            Condition::ReleaseGroup {
                pattern: "GROUP".into(),
            },
            false,
            false,
        );
        let no = spec(
            Condition::ReleaseGroup {
                pattern: "NO".into(),
            },
            false,
            false,
        );
        for (specs, expected) in [
            (vec![yes.clone(), no.clone()], true),
            (
                vec![
                    yes.clone(),
                    Specification {
                        required: true,
                        ..no.clone()
                    },
                ],
                false,
            ),
            (vec![no.clone()], false),
            (
                vec![
                    yes.clone(),
                    spec(
                        Condition::Year {
                            min: 2000,
                            max: 2001,
                        },
                        false,
                        false,
                    ),
                ],
                false,
            ),
        ] {
            let result = evaluate(
                vec![format(specs)],
                MediaDomain::Movies,
                e.clone(),
                Some(1),
                vec![FormatScore {
                    format_id: 1,
                    score: i32::MAX,
                }],
            )
            .await
            .unwrap();
            assert_eq!(!result.format_ids.is_empty(), expected);
            assert_eq!(result.score, if expected { i64::from(i32::MAX) } else { 0 });
        }
        assert!(
            compile_specs(vec![spec(
                Condition::ReleaseTitle {
                    pattern: "(".into()
                },
                false,
                false
            )])
            .await
            .is_err()
        );
        let costly = spec(
            Condition::ReleaseTitle {
                pattern: r"^(a|aa)+(?=a)$".into(),
            },
            false,
            false,
        );
        let mut attack = e;
        attack.title = format!("{}!", "a".repeat(1000));
        attack.year = None;
        assert_eq!(
            matches_spec(&costly, MediaDomain::Tv, &attack, None).unwrap_err(),
            "custom_format_regex_failed"
        );
    }
    #[tokio::test]
    async fn all_native_quality_facts_are_read_from_immutable_catalog() {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-quality-facts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = crate::db::Database::open_local(path.join("db"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        let mut rows = c
            .query(
                "SELECT media_type,name,source,resolution,modifier FROM quality_definitions",
                (),
            )
            .await
            .unwrap();
        let mut count = 0;
        while let Some(row) = rows.next().await.unwrap() {
            let tv = row.get::<String>(0).unwrap() == "tv";
            let name: String = row.get(1).unwrap();
            let source: String = row.get(2).unwrap();
            let resolution: i64 = row.get(3).unwrap();
            let modifier: Option<String> = row.get(4).unwrap();
            let mut e = evidence();
            populate_quality(&c, &mut e, Some(&name), tv).await.unwrap();
            // Every catalog row, including DVD/Raw-HD/BR-DISK/Remux, retains its
            // authoritative resolution and modifier rather than guessing from display names.
            assert_eq!(i64::from(e.resolution), resolution, "{name}");
            let source_names = if tv {
                vec![
                    "unknown",
                    "television",
                    "televisionRaw",
                    "web",
                    "webRip",
                    "dvd",
                    "bluray",
                    "blurayRaw",
                ]
            } else {
                vec![
                    "unknown",
                    "cam",
                    "telesync",
                    "telecine",
                    "workprint",
                    "dvd",
                    "tv",
                    "webdl",
                    "webrip",
                    "bluray",
                ]
            };
            assert_eq!(source_names[e.source as usize], source, "{name}");
            assert_eq!(
                ["none", "regional", "screener", "rawhd", "brdisk", "remux"][e.modifier as usize],
                modifier.as_deref().unwrap_or("none"),
                "{name}"
            );
            count += 1;
        }
        assert!(count > 50);
        drop(rows);
        c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'Series','/tv'); INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/Current.mkv'); INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies'); INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/Current.mkv'); INSERT INTO file_metadata(media_type,episode_file_id)VALUES('tv',1); INSERT INTO file_metadata(media_type,movie_file_id)VALUES('movies',1);").await.unwrap();
        for (title, original_path, expected) in [
            (Some("   "), "/original/Original.mkv", "Original.mkv"),
            (Some("\t"), "/original/   ", "Current.mkv"),
            (None, "   ", "Current.mkv"),
            (
                Some("  Actual release  "),
                "/original/Original.mkv",
                "  Actual release  ",
            ),
        ] {
            c.execute(
                "UPDATE file_metadata SET original_release_title=?,original_file_path=CASE WHEN media_type='movies' THEN ? ELSE NULL END",
                params![title, original_path],
            )
            .await
            .unwrap();
            for media in [MediaDomain::Tv, MediaDomain::Movies] {
                // TV original paths are currently absent by schema contract; its blank
                // scene title falls directly through to the current basename.
                let expected =
                    if media == MediaDomain::Tv && title.is_none_or(|s| s.trim().is_empty()) {
                        "Current.mkv"
                    } else {
                        expected
                    };
                assert_eq!(existing(&c, media, 1).await.unwrap().title, expected);
            }
        }
        drop(c);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn unknown_original_custom_format_uses_sentinel_and_indexer_factors_are_charged_fractions() {
        let e = evidence();
        // Pinned CF semantics compare the Original sentinel if metadata is unknown;
        // this is intentionally separate from the movie profile's known-original guard.
        for (except, negate, want) in [
            (false, false, false),
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            assert_eq!(
                matches_spec(
                    &spec(
                        Condition::Language {
                            value: -2,
                            except_language: except
                        },
                        false,
                        negate
                    ),
                    MediaDomain::Movies,
                    &e,
                    Some(0)
                )
                .unwrap(),
                want
            );
        }
        for tv in [true, false] {
            for (factor, flag) in [
                (0.75, if tv { 32 } else { 256 }),
                (0.25, if tv { 64 } else { 512 }),
            ] {
                // Protocol volume factors describe the amount COUNTED, not the savings.
                let title = if tv {
                    "Harbor.S01E01.1080p.WEB-DL"
                } else {
                    "Harbor.2020.1080p.WEB-DL"
                };
                let xml = format!(
                    r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>owned-test</guid><pubDate>Mon, 01 Jan 2024 12:00:00 +0000</pubDate><link>https://example.invalid/owned</link><torznab:attr name="downloadvolumefactor" value="{factor}"/></item></channel></rss>"#
                );
                let r = crate::providers::indexer::parse_page(
                    &xml,
                    0,
                    100,
                    true,
                    if tv {
                        MediaDomain::Tv
                    } else {
                        MediaDomain::Movies
                    },
                )
                .unwrap()
                .items
                .remove(0);
                let parsed = crate::search::parser::parse(title, tv).unwrap();
                assert_eq!(release(&r, &parsed, tv).indexer_flags, Some(flag));
            }
        }
    }
    #[test]
    fn original_names_are_not_language_evidence_and_domains_differ() {
        assert_eq!(
            simplified_title("Harbor (2020) 1080p WEB-DL", Some(2020)),
            "A Movie (2020) 1080p WEB-DL"
        );
        assert_eq!(
            simplified_title("1984.1984.1080p.WEB-DL", Some(1984)),
            "A Movie.1984.1080p.WEB-DL"
        );
        assert!(regex("((?=a)){1000000000}").is_err());
        assert!(regex("((a{4096}){4096}){4096}").is_err());
        assert_eq!(
            simplified_title("Harbor.2020.English.1080p.2020-GROUP", Some(2020)),
            "A Movie.2020.English.1080p.2020-GROUP"
        );
        let p = crate::search::parser::parse("English.Patient.1996.1080p.WEB-DL", false).unwrap();
        assert_eq!(
            parsed("English.Patient.1996.1080p.WEB-DL", &p, false, None).languages,
            None
        );
        assert_eq!(language("Macedonian", true), Some(45));
        assert_eq!(language("Macedonian", false), Some(46));
        assert_eq!(
            simplified_title("Harbor.2020.English.1080p", Some(2020)),
            "A Movie.2020.English.1080p"
        );
    }
}
