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
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
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
    let positive = |s: &str| {
        s.parse::<i64>()
            .ok()
            .filter(|v| (1..=i64::from(i32::MAX)).contains(v))
            .ok_or("invalid_revision")
    };
    if let Some(version) = attached {
        value.version = positive(version)?;
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
            marker = true;
        } else if token == "real" {
            value.real += 1;
            marker = true;
        } else if let Some(n) = token
            .strip_prefix("repack")
            .or_else(|| token.strip_prefix("rerip"))
        {
            if n.is_empty() || n.bytes().all(|c| c.is_ascii_digit()) {
                let number = if n.is_empty() { 1 } else { positive(n)? };
                value.version = value.version.max(
                    number
                        .checked_add(1)
                        .filter(|v| *v <= i64::from(i32::MAX))
                        .ok_or("invalid_revision")?,
                );
                value.is_repack = true;
                marker = true;
            }
        } else if tv {
            if let Some(n) = token
                .strip_prefix('v')
                .filter(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
            {
                value.version = value.version.max(positive(n)?);
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
    if boundary == 0 {
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
    let identity_tokens = match &parsed_numbering {
        Some(Numbering::Daily { .. }) => 3,
        Some(Numbering::Absolute { .. }) => 2,
        _ => 1,
    };
    let technical_start = Some(
        tokens
            .get(boundary + identity_tokens)
            .map_or(value.len(), |token| {
                token.as_ptr() as usize - value.as_ptr() as usize
            }),
    );
    let attached = if tv {
        lower
            .get(boundary + identity_tokens - 1)
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
