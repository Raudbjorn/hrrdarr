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
    pub revision: u8,
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
                if let Some(episode) = tokens.get(i + 1).and_then(|s| number(s)) {
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
    Ok(ParsedRelease {
        title,
        year,
        numbering: parsed_numbering,
        quality_name,
        edition,
        revision: if has("proper") || has("repack") { 2 } else { 1 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
