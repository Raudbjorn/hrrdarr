//! Independently implemented token grammar. Unrecognized numbering never becomes a guess.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Numbering {
    Episodes { season: i64, episodes: Vec<i64> },
    Season { season: i64 },
    Daily { date: String },
    Absolute { episode: i64 },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct ParsedRelease {
    pub title: String,
    pub year: Option<i64>,
    pub numbering: Option<Numbering>,
    pub quality_name: Option<String>,
    pub edition: Option<String>,
    #[serde(default, deserialize_with = "revision_field")]
    pub revision: Option<crate::media_files::FileRevision>,
    #[serde(default)]
    pub revision_marker: bool,
    #[serde(default)]
    pub technical_start: Option<usize>,
}
fn revision_field<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<crate::media_files::FileRevision>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    if value.is_null() || matches!(value.as_u64(), Some(1 | 2)) {
        // Historical numeric facts cannot distinguish PROPER from REPACK or REAL.
        return Ok(None);
    }
    let revision: crate::media_files::FileRevision =
        serde_json::from_value(value).map_err(serde::de::Error::custom)?;
    revision.validate().map_err(serde::de::Error::custom)?;
    Ok(Some(revision))
}
fn version_suffix(token: &str) -> (&str, Option<&str>) {
    match token.rsplit_once('v') {
        Some((base, version))
            if base.bytes().last().is_some_and(|c| c.is_ascii_digit())
                && !version.is_empty()
                && version.bytes().all(|c| c.is_ascii_digit()) =>
        {
            (base, Some(version))
        }
        _ => (token, None),
    }
}
fn revision_tokens(
    tokens: &[String],
    attached: Option<&str>,
    tv: bool,
) -> Result<(crate::media_files::FileRevision, bool), &'static str> {
    let mut value = crate::media_files::FileRevision {
        version: 1,
        real: 0,
        is_repack: false,
    };
    let mut marker = false;
    let mut version_specified = attached.is_some();
    let revision_number = |s: &str, minimum: i64| {
        s.parse::<i64>()
            .ok()
            .filter(|v| (minimum..=i64::from(i32::MAX)).contains(v))
            .ok_or("invalid_revision")
    };
    if let Some(version) = attached {
        value.version = revision_number(version, 0)?;
        marker = true;
    }
    for token in tokens {
        if token == "-" {
            break;
        }
        // Preserve recognized source compounds, then stop at the group suffix.
        // Dotted words inside that suffix cannot become technical revision facts.
        let source_compound = ["web-dl", "blu-ray"]
            .into_iter()
            .find(|source| token.as_str() == *source || token.starts_with(&format!("{source}-")));
        let (token, group_follows) = if let Some(source) = source_compound {
            (source, token.len() > source.len())
        } else {
            token
                .split_once('-')
                .map_or((token.as_str(), false), |(head, _)| (head, true))
        };
        if token == "proper" {
            value.version = value.version.max(2);
            version_specified = true;
            marker = true;
        } else if token == "real" {
            value.real += 1;
            marker = true;
        } else if let Some(n) = token
            .strip_prefix("repack")
            .or_else(|| token.strip_prefix("rerip"))
        {
            if n.is_empty() || n.bytes().all(|c| c.is_ascii_digit()) {
                let number = if n.is_empty() {
                    1
                } else {
                    revision_number(n, 1)?
                };
                value.version = value.version.max(
                    number
                        .checked_add(1)
                        .filter(|v| *v <= i64::from(i32::MAX))
                        .ok_or("invalid_revision")?,
                );
                value.is_repack = true;
                version_specified = true;
                marker = true;
            }
        } else if tv {
            if let Some(n) = token
                .strip_prefix('v')
                .filter(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
            {
                let version = revision_number(n, 0)?;
                // An explicit v0 replaces the implicit baseline, but never a higher marker.
                value.version = if version_specified {
                    value.version.max(version)
                } else {
                    version
                };
                version_specified = true;
                marker = true;
            }
        }
        if group_follows {
            break;
        }
    }
    value.validate().map_err(|_| "invalid_revision")?;
    Ok((value, marker))
}
pub fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn number(value: &str) -> Option<i64> {
    (!value.is_empty() && value.len() <= 4 && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse::<i64>().ok())
        .flatten()
}
fn numbering(value: &str) -> Option<Numbering> {
    let value = value.to_ascii_lowercase();
    let (value, _) = version_suffix(&value);
    let tail = value.strip_prefix('s')?;
    let mut parts = tail.split('e');
    let season = number(parts.next()?)?;
    let mut episodes = Vec::new();
    for part in parts {
        episodes.push(number(part)?);
    }
    if episodes.len() > 32 {
        return None;
    }
    if episodes.is_empty() {
        Some(Numbering::Season { season })
    } else if episodes.windows(2).any(|p| p[0] >= p[1]) {
        None
    } else {
        Some(Numbering::Episodes { season, episodes })
    }
}
pub fn parse(value: &str, tv: bool) -> Result<ParsedRelease, &'static str> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err("invalid_release_title");
    }
    let tokens: Vec<&str> = value
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '(' | ')' | '[' | ']'))
        .filter(|s| !s.is_empty())
        .collect();
    let lower: Vec<String> = tokens.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut boundary = tokens.len();
    let mut parsed_numbering = None;
    let mut year = None;
    if tv {
        for (i, token) in lower.iter().enumerate() {
            if let Some(n) = numbering(token) {
                if parsed_numbering.is_some() {
                    return Err("ambiguous_numbering");
                }
                boundary = i;
                parsed_numbering = Some(n);
            }
        }
        if parsed_numbering.is_none() {
            for i in 1..tokens.len().saturating_sub(2) {
                let date = format!("{}-{}-{}", tokens[i], tokens[i + 1], tokens[i + 2]);
                if tokens[i].len() == 4
                    && chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok()
                {
                    boundary = i;
                    parsed_numbering = Some(Numbering::Daily { date });
                    break;
                }
            }
        }
        if parsed_numbering.is_none() {
            if let Some(i) = tokens.iter().position(|s| *s == "-") {
                if let Some(episode) = lower.get(i + 1).and_then(|s| number(version_suffix(s).0)) {
                    boundary = i;
                    parsed_numbering = Some(Numbering::Absolute { episode });
                }
            }
        }
        if parsed_numbering.is_none() {
            return Err("unsupported_numbering");
        }
    } else {
        if lower.iter().any(|s| numbering(s).is_some()) {
            return Err("wrong_media_numbering");
        }
        // The rightmost year before technical metadata handles numeric movie titles.
        let technical = lower
            .iter()
            .position(|s| {
                matches!(
                    s.as_str(),
                    "480p"
                        | "576p"
                        | "720p"
                        | "1080p"
                        | "2160p"
                        | "bluray"
                        | "web-dl"
                        | "webrip"
                        | "hdtv"
                )
            })
            .unwrap_or(tokens.len());
        for (i, token) in tokens.iter().enumerate().take(technical).skip(1) {
            if let Some(y) = number(token).filter(|y| (1870..=2199).contains(y)) {
                year = Some(y);
                boundary = i;
            }
        }
        if year.is_none() {
            return Err("missing_movie_year");
        }
    }
    finish_parse(
        value,
        tv,
        tokens,
        lower,
        boundary,
        parsed_numbering,
        year,
        None,
        false,
    )
}
fn finish_parse(
    value: &str,
    tv: bool,
    tokens: Vec<&str>,
    lower: Vec<String>,
    boundary: usize,
    parsed_numbering: Option<Numbering>,
    year: Option<i64>,
    end: Option<usize>,
    allow_missing_title: bool,
) -> Result<ParsedRelease, &'static str> {
    if boundary == 0 && !allow_missing_title {
        return Err("missing_release_title");
    }
    let title = tokens[..boundary].join(" ");
    let resolutions: std::collections::BTreeSet<_> = lower
        .iter()
        .filter_map(|s| match s.as_str() {
            "480p" => Some(480),
            "576p" => Some(576),
            "720p" => Some(720),
            "1080p" => Some(1080),
            "2160p" => Some(2160),
            _ => None,
        })
        .collect();
    if resolutions.len() > 1 {
        return Err("ambiguous_quality");
    }
    let resolution = resolutions.first().copied();
    let has = |name: &str| lower.iter().any(|s| s == name);
    let sources = [
        ("WEBDL", has("web-dl") || has("webdl")),
        ("WEBRip", has("webrip")),
        ("Bluray", has("bluray") || has("blu-ray")),
        ("HDTV", has("hdtv")),
    ];
    let found: Vec<_> = sources.iter().filter(|(_, yes)| *yes).collect();
    if found.len() > 1 {
        return Err("ambiguous_quality");
    }
    let quality_name = if found.len() == 1 {
        resolution.map(|r| {
            if has("remux") && found[0].0 == "Bluray" {
                if tv {
                    format!("Bluray-{r}p Remux")
                } else {
                    format!("Remux-{r}p")
                }
            } else {
                format!("{}-{r}p", found[0].0)
            }
        })
    } else {
        None
    };
    let edition = if has("extended") {
        Some("Extended".into())
    } else if has("unrated") {
        Some("Unrated".into())
    } else {
        None
    };
    let identity_tokens =
        end.map(|end| end - boundary)
            .unwrap_or_else(|| match &parsed_numbering {
                Some(Numbering::Daily { .. }) => 3,
                Some(Numbering::Absolute { .. }) => 2,
                _ => 1,
            });
    let technical_start = Some(
        tokens
            .get(boundary + identity_tokens)
            .map_or(value.len(), |token| {
                token.as_ptr() as usize - value.as_ptr() as usize
            }),
    );
    let attached = if tv {
        lower
            .get((boundary + identity_tokens).saturating_sub(1))
            .and_then(|token| version_suffix(token).1)
    } else {
        None
    };
    for (index, token) in tokens.iter().enumerate().skip(boundary + identity_tokens) {
        let start = token.as_ptr() as usize - value.as_ptr() as usize;
        let end = start + token.len();
        if value.as_bytes().get(start.wrapping_sub(1)) == Some(&b'[')
            && value.as_bytes().get(end) == Some(&b']')
            && revision_tokens(&lower[index..index + 1], None, tv)?.1
        {
            // A bracketed revision-like group and a bracketed revision marker are
            // indistinguishable in this bounded grammar. Never assert either fact.
            return Err("ambiguous_revision_marker");
        }
    }
    let (revision, revision_marker) =
        revision_tokens(&lower[boundary + identity_tokens..], attached, tv)?;
    Ok(ParsedRelease {
        technical_start,
        title,
        year,
        numbering: parsed_numbering,
        quality_name,
        edition,
        revision: Some(revision),
        revision_marker,
    })
}

/// Complete downloaded-file numbering. Release search/rescan keep their existing
/// public grammar; this internal representation cannot be mistaken for one anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DownloadedNumbering {
    Episodes { season: i64, episodes: Vec<i64> },
    Daily { dates: Vec<String> },
    Absolute { episodes: Vec<i64> },
    Season { season: i64 },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParsedFile {
    pub parsed: ParsedRelease,
    pub numbering: Option<DownloadedNumbering>,
}
pub(crate) const MAX_DOWNLOADED_MEMBERS: usize = 20_000;
fn scan_number(s: &str) -> Result<i64, &'static str> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid_numbering");
    }
    s.parse::<i64>()
        .ok()
        .filter(|n| *n <= i64::from(i32::MAX))
        .ok_or("invalid_numbering")
}
/// `-` is an inclusive range, `+` and repeated E/x are explicit list boundaries.
fn scan_numbers(raw: &str) -> Result<Vec<i64>, &'static str> {
    let mut values = std::collections::BTreeSet::new();
    let raw = raw.to_ascii_lowercase();
    let (raw, _) = version_suffix(&raw);
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut previous = None;
    let mut range = false;
    while cursor < bytes.len() {
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if start == cursor {
            return Err("invalid_numbering");
        }
        let current = scan_number(&raw[start..cursor])?;
        if range {
            let first = previous.ok_or("invalid_numbering")?;
            if current < first || current - first >= MAX_DOWNLOADED_MEMBERS as i64 {
                return Err("numbering_limit");
            }
            for number in first..=current {
                values.insert(number);
            }
        } else {
            values.insert(current);
        }
        if values.len() > MAX_DOWNLOADED_MEMBERS {
            return Err("numbering_limit");
        }
        previous = Some(current);
        if cursor == bytes.len() {
            break;
        }
        range = match bytes[cursor] {
            b'-' => true,
            b'+' | b'e' | b'x' => false,
            _ => return Err("invalid_numbering"),
        };
        cursor += 1;
        if cursor < bytes.len() && matches!(bytes[cursor], b'e' | b'x') {
            cursor += 1;
        }
        if cursor == bytes.len() {
            return Err("invalid_numbering");
        }
    }
    if values.is_empty() {
        return Err("invalid_numbering");
    }
    Ok(values.into_iter().collect())
}
fn scan_coordinate(raw: &str) -> Result<Option<(i64, Option<Vec<i64>>)>, &'static str> {
    let lower = raw.to_ascii_lowercase();
    let (raw, _) = version_suffix(&lower);
    if let Some(rest) = raw
        .strip_prefix('s')
        .filter(|s| s.as_bytes().first().is_some_and(u8::is_ascii_digit))
    {
        if let Some((season, episodes)) = rest.split_once('e') {
            return Ok(Some((scan_number(season)?, Some(scan_numbers(episodes)?))));
        }
        return Ok(Some((scan_number(rest)?, None)));
    }
    if let Some((season, episodes)) = raw
        .split_once('x')
        .filter(|(s, _)| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(Some((scan_number(season)?, Some(scan_numbers(episodes)?))));
    }
    Ok(None)
}
/// Reuses the release parser's technical fact parser without prepending an invented
/// owner title. Missing filename identity remains explicit for folder/receipt matching.
pub(crate) fn parse_downloaded(value: &str, tv: bool) -> Result<ParsedFile, &'static str> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err("invalid_release_title");
    }
    let tokens: Vec<&str> = value
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '(' | ')' | '[' | ']'))
        .filter(|s| !s.is_empty())
        .collect();
    let lower = tokens
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if !tv {
        // Preserve ordinary movie parsing; downloaded folder/receipt identity can
        // supply an absent year/title, but never overwrite a contradictory one.
        if let Ok(parsed) = parse(value, false) {
            return Ok(ParsedFile {
                parsed,
                numbering: None,
            });
        }
        for token in &lower {
            if scan_coordinate(token)?.is_some() {
                return Err("wrong_media_numbering");
            }
        }
        let technical = lower
            .iter()
            .position(|s| {
                matches!(
                    s.as_str(),
                    "480p"
                        | "576p"
                        | "720p"
                        | "1080p"
                        | "2160p"
                        | "web-dl"
                        | "webdl"
                        | "webrip"
                        | "bluray"
                        | "hdtv"
                )
            })
            .unwrap_or(tokens.len());
        let year = (0..technical).rev().find_map(|i| {
            scan_number(tokens[i])
                .ok()
                .filter(|y| (1870..=2199).contains(y))
                .map(|y| (i, y))
        });
        let boundary = year.map_or(technical, |(i, _)| i);
        let end = year.map_or(technical, |(i, _)| i + 1);
        let parsed = finish_parse(
            value,
            false,
            tokens,
            lower,
            boundary,
            None,
            year.map(|(_, y)| y),
            Some(end),
            true,
        )?;
        return Ok(ParsedFile {
            parsed,
            numbering: None,
        });
    }
    let mut boundary = None;
    let mut end = 0;
    let mut season = None;
    let mut episodes = std::collections::BTreeSet::new();
    let mut pack = false;
    for (i, token) in lower.iter().enumerate() {
        if let Some((s, numbers)) = scan_coordinate(token)? {
            if season.is_some_and(|old| old != s) {
                return Err("cross_season_numbering");
            }
            season = Some(s);
            boundary.get_or_insert(i);
            end = i + 1;
            if let Some(numbers) = numbers {
                episodes.extend(numbers);
            } else {
                pack = true;
            }
        } else if season.is_some()
            && i == end
            && token.starts_with('e')
            && token.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
        {
            episodes.extend(scan_numbers(&token[1..])?);
            end = i + 1;
        }
        if episodes.len() > MAX_DOWNLOADED_MEMBERS {
            return Err("numbering_limit");
        }
    }
    if let Some(s) = season {
        if pack && !episodes.is_empty() {
            return Err("ambiguous_numbering");
        }
        let complete = if pack {
            DownloadedNumbering::Season { season: s }
        } else {
            DownloadedNumbering::Episodes {
                season: s,
                episodes: episodes.iter().copied().collect(),
            }
        };
        let compatible = if pack {
            Numbering::Season { season: s }
        } else {
            Numbering::Episodes {
                season: s,
                episodes: episodes.into_iter().collect(),
            }
        };
        let parsed = finish_parse(
            value,
            true,
            tokens,
            lower,
            boundary.unwrap(),
            Some(compatible),
            None,
            Some(end),
            true,
        )?;
        return Ok(ParsedFile {
            parsed,
            numbering: Some(complete),
        });
    }
    let mut dates = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < tokens.len() {
        let (date, consumed) = if tokens[i].len() == 10
            && chrono::NaiveDate::parse_from_str(tokens[i], "%Y-%m-%d").is_ok()
        {
            (Some(tokens[i].to_owned()), 1)
        } else if i + 2 < tokens.len() && tokens[i].len() == 4 {
            let date = format!("{}-{}-{}", tokens[i], tokens[i + 1], tokens[i + 2]);
            if chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok() {
                (Some(date), 3)
            } else {
                (None, 1)
            }
        } else {
            (None, 1)
        };
        if let Some(date) = date {
            boundary.get_or_insert(i);
            end = i + consumed;
            dates.insert(date);
        }
        i += consumed;
    }
    if !dates.is_empty() {
        let compatible = if dates.len() == 1 {
            Some(Numbering::Daily {
                date: dates.first().unwrap().clone(),
            })
        } else {
            None
        };
        let parsed = finish_parse(
            value,
            true,
            tokens,
            lower,
            boundary.unwrap(),
            compatible,
            None,
            Some(end),
            true,
        )?;
        return Ok(ParsedFile {
            parsed,
            numbering: Some(DownloadedNumbering::Daily {
                dates: dates.into_iter().collect(),
            }),
        });
    }
    // Absolute syntax explicitly separates the title and numeric span with ` - `;
    // a number-only filename is also identifiable using a trusted folder/receipt.
    let start = if let Some(i) = tokens.iter().position(|s| *s == "-") {
        i + 1
    } else {
        0
    };
    if let Some(raw) = lower
        .get(start)
        .filter(|s| s.as_bytes().first().is_some_and(u8::is_ascii_digit) && !s.ends_with('p'))
    {
        let numbers = scan_numbers(raw)?;
        let compatible = if numbers.len() == 1 {
            Some(Numbering::Absolute {
                episode: numbers[0],
            })
        } else {
            None
        };
        let title_end = if start > 0 { start - 1 } else { 0 };
        let parsed = finish_parse(
            value,
            true,
            tokens,
            lower,
            title_end,
            compatible,
            None,
            Some(start + 1),
            true,
        )?;
        return Ok(ParsedFile {
            parsed,
            numbering: Some(DownloadedNumbering::Absolute { episodes: numbers }),
        });
    }
    // A folder may carry identity or technical facts without numbering. Keep that
    // absence explicit; the complete-file resolver must obtain a real member set.
    let boundary = lower
        .iter()
        .position(|s| {
            matches!(
                s.as_str(),
                "480p"
                    | "576p"
                    | "720p"
                    | "1080p"
                    | "2160p"
                    | "web-dl"
                    | "webdl"
                    | "webrip"
                    | "bluray"
                    | "hdtv"
            )
        })
        .unwrap_or(tokens.len());
    let parsed = finish_parse(
        value,
        true,
        tokens,
        lower,
        boundary,
        None,
        None,
        Some(boundary),
        true,
    )?;
    Ok(ParsedFile {
        parsed,
        numbering: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_facts_and_legacy_decode() {
        for (suffix, version, real, repack) in [
            ("", 1, 0, false),
            ("PROPER", 2, 0, false),
            ("REPACK", 2, 0, true),
            ("RERIP2", 3, 0, true),
            ("REAL.REAL.PROPER", 2, 2, false),
            ("v3", 3, 0, false),
            ("v0", 0, 0, false),
            ("REAL.v0", 0, 1, false),
            ("v0.PROPER", 2, 0, false),
            ("PROPER.v0", 2, 0, false),
            ("v0.REPACK", 2, 0, true),
            ("REPACK.v0", 2, 0, true),
            ("v1.v0", 1, 0, false),
            ("v0.v1", 1, 0, false),
        ] {
            let parsed = parse(
                &format!("Real.Proper.Harbor.S01E02.1080p.WEB-DL.{suffix}"),
                true,
            )
            .unwrap();
            let revision = parsed.revision.as_ref().unwrap();
            assert_eq!(
                (revision.version, revision.real, revision.is_repack),
                (version, real, repack)
            );
            assert_eq!(parsed.revision_marker, !suffix.is_empty());
        }
        let parsed = parse("Harbor - 023v4 1080p WEB-DL-GROUP", true).unwrap();
        assert_eq!(parsed.numbering, Some(Numbering::Absolute { episode: 23 }));
        assert_eq!(parsed.revision.unwrap().version, 4);
        for title in [
            "Harbor - 023v0 1080p WEB-DL-GROUP",
            "Harbor.S01E02v0.1080p.WEB-DL",
        ] {
            let parsed = parse(title, true).unwrap();
            assert_eq!(parsed.revision.unwrap().version, 0);
            assert!(parsed.revision_marker);
        }
        for suffix in ["v2147483648", "REPACK0"] {
            assert_eq!(
                parse(&format!("Harbor.S01E02.1080p.{suffix}"), true).unwrap_err(),
                "invalid_revision"
            );
        }
        assert!(
            !parse("Proper.2020.1080p.WEB-DL-PROPER", false)
                .unwrap()
                .revision_marker
        );
        for suffix in [
            "WEB-DL-GROUP.PROPER",
            "Blu-Ray-REAL.REPACK2",
            "HDTV-TEAM.v3",
        ] {
            assert!(
                !parse(&format!("Harbor.S01E02.1080p.{suffix}"), true)
                    .unwrap()
                    .revision_marker,
                "{suffix}"
            );
        }
        for source in ["WEB-DL", "Blu-Ray"] {
            assert!(
                parse(
                    &format!("Harbor.S01E02.1080p.{source}.PROPER-GROUP.REAL"),
                    true
                )
                .unwrap()
                .revision_marker
            );
            assert_eq!(
                parse(
                    &format!("Harbor.S01E02.1080p.{source}.PROPER-GROUP.REAL"),
                    true
                )
                .unwrap()
                .revision
                .unwrap()
                .real,
                0
            );
        }
        assert!(parse("Harbor.S01E02.REPACK2147483647", true).is_err());
        assert!(
            !parse("[REAL] Harbor.S01E02.1080p.WEB-DL - PROPER", true)
                .unwrap()
                .revision_marker
        );
        assert_eq!(
            parse("Harbor.S01E02.1080p.WEB-DL.[REPACK]", true).unwrap_err(),
            "ambiguous_revision_marker"
        );

        let mut old =
            serde_json::to_value(parse("Harbor.2020.1080p.WEB-DL", false).unwrap()).unwrap();
        old["revision"] = serde_json::json!(2);
        old.as_object_mut().unwrap().remove("revision_marker");
        let decoded: ParsedRelease = serde_json::from_value(old).unwrap();
        assert!(decoded.revision.is_none());
        assert!(!decoded.revision_marker);
    }
    #[test]
    fn independent_tv_and_movie_corpora() {
        for name in [
            "Harbor.S01E02.1080p.WEB-DL",
            "Harbor S00E01 720p HDTV",
            "Harbor.S01E02E03.1080p.WEBRip",
            "Harbor S02 1080p Bluray",
            "Harbor.2026.09.24.720p.HDTV",
            "Harbor - 023 1080p WEB-DL",
        ] {
            assert!(parse(name, true).is_ok(), "{name}");
        }
        for name in [
            "Harbor.1982.1080p.Bluray",
            "Harbor (2011) Extended 2160p WEB-DL",
            "1917.2019.1080p.Bluray",
            "2001.A.Space.Voyage.1968.1080p.Bluray",
        ] {
            assert!(parse(name, false).is_ok(), "{name}");
        }
        assert_eq!(
            parse("1917.2019.1080p.Bluray", false).unwrap().title,
            "1917"
        );
        assert_eq!(
            parse("Harbor.S01E02.1080p.WEB-DL", false).unwrap_err(),
            "wrong_media_numbering"
        );
        for name in [
            "Harbor S01E02E01 1080p WEB-DL",
            "Harbor S01E02 S02E04",
            "Harbor 1080p WEB-DL",
        ] {
            assert!(parse(name, true).is_err());
        }
        assert!(parse("Harbor.1080p.WEB-DL", false).is_err());
        let multi = parse("Harbor.S01E02E03.1080p.WEB-DL", true).unwrap();
        assert_eq!(
            multi.numbering,
            Some(Numbering::Episodes {
                season: 1,
                episodes: vec![2, 3]
            })
        );
        assert_eq!(multi.quality_name.as_deref(), Some("WEBDL-1080p"));
        assert_eq!(
            parse("Harbor.2026.09.24.720p.HDTV", true)
                .unwrap()
                .numbering,
            Some(Numbering::Daily {
                date: "2026-09-24".into()
            })
        );
        assert_eq!(
            parse("Harbor - 023 1080p WEB-DL", true).unwrap().numbering,
            Some(Numbering::Absolute { episode: 23 })
        );
        assert_eq!(
            parse("Harbor.S01E02.1080p.720p.WEB-DL", true).unwrap_err(),
            "ambiguous_quality"
        );
    }
}

#[cfg(test)]
mod downloaded_tests {
    use super::*;
    #[test]
    fn downloaded_complete_sets_do_not_change_release_grammar() {
        for (input, expected) in [
            ("Harbor.S01E01-E03.1080p.WEB-DL", vec![1, 2, 3]),
            ("Harbor.S01E03E01E03.1080p.WEB-DL", vec![1, 3]),
            ("Harbor.S01E01-02+E04.1080p.WEB-DL", vec![1, 2, 4]),
            ("Harbor.1x01-03.1080p.WEB-DL", vec![1, 2, 3]),
            ("Harbor S01E01 - S01E03 1080p WEB-DL", vec![1, 3]),
        ] {
            let p = parse_downloaded(input, true).unwrap();
            assert_eq!(
                p.numbering,
                Some(DownloadedNumbering::Episodes {
                    season: 1,
                    episodes: expected
                }),
                "{input}"
            );
            assert_eq!(p.parsed.title, "Harbor");
            assert_eq!(p.parsed.quality_name.as_deref(), Some("WEBDL-1080p"));
        }
        assert!(parse("Harbor.S01E01-E03.1080p.WEB-DL", true).is_err());
        for input in [
            "Harbor.S01E03-01",
            "Harbor.S01E01-20002",
            "Harbor.S01E01.S02E01",
            "Harbor.S01E01+",
            "Harbor.S01E2147483648",
        ] {
            assert!(parse_downloaded(input, true).is_err(), "{input}");
        }
    }
    #[test]
    fn downloaded_daily_absolute_and_missing_identity_are_explicit() {
        let p = parse_downloaded("Harbor.2026.09.24.2026.09.25.720p.HDTV", true).unwrap();
        assert_eq!(
            p.numbering,
            Some(DownloadedNumbering::Daily {
                dates: vec!["2026-09-24".into(), "2026-09-25".into()]
            })
        );
        assert!(p.parsed.numbering.is_none()); // Never collapse an extended set to one public member.
        let p = parse_downloaded("Harbor - 047-049v0 1080p WEB-DL", true).unwrap();
        assert_eq!(
            p.numbering,
            Some(DownloadedNumbering::Absolute {
                episodes: vec![47, 48, 49]
            })
        );
        assert_eq!(p.parsed.revision.unwrap().version, 0);
        assert!(
            parse_downloaded("S01E01.1080p.WEB-DL", true)
                .unwrap()
                .parsed
                .title
                .is_empty()
        );
        assert!(
            parse_downloaded("Harbor", true)
                .unwrap()
                .numbering
                .is_none()
        );
        assert_eq!(
            parse_downloaded("Harbor.1080p.WEB-DL", false)
                .unwrap()
                .parsed
                .year,
            None
        );
        assert_eq!(
            parse_downloaded("Harbor.S01E01", false).unwrap_err(),
            "wrong_media_numbering"
        );
        assert!(parse_downloaded("Harbor.S01Ebad", false).is_err());
    }
}
