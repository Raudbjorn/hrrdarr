//! Minimal RFC 5545 iCalendar writer for the native calendar feed. Only fixed property names
//! are emitted; every externally sourced value goes through TEXT escaping and every content
//! line is folded at 75 octets without splitting a UTF-8 sequence. Lines end in CRLF.
use chrono::{DateTime, NaiveDate, Utc};

/// RFC 5545 section 3.1: content lines SHOULD NOT exceed 75 octets, excluding the line break.
pub(super) const MAX_LINE_OCTETS: usize = 75;
/// Longest duration written for a timed event; larger stored runtimes omit DURATION instead.
pub(super) const MAX_DURATION_MINUTES: i64 = 7 * 24 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Confirmed,
    Tentative,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum When {
    /// Known UTC instant; `minutes` is the known runtime (absent: no DURATION is written).
    Timed {
        start: DateTime<Utc>,
        minutes: Option<i64>,
    },
    /// Floating all-day date with the exclusive next-day DTEND.
    AllDay(NaiveDate),
}
#[derive(Clone, Debug)]
pub(super) struct Event {
    /// Stable identity: typed entity plus release type, never a date.
    pub uid: String,
    pub when: When,
    pub summary: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub status: Status,
}

/// TEXT escaping (RFC 5545 section 3.3.11): backslash, semicolon, comma and line breaks are
/// escaped; other control characters (except HTAB) are not representable and become spaces.
pub(super) fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push('\t'),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Appends one folded content line. Continuation lines start with one space, which counts
/// toward their 75 octets; multi-octet characters are never split.
pub(super) fn fold_into(out: &mut String, line: &str) {
    let mut width = 0usize;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > MAX_LINE_OCTETS {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out.push_str("\r\n");
}

fn utc(value: &DateTime<Utc>) -> String {
    // DATE-TIME has whole-second precision; fractional stored seconds are truncated.
    value.format("%Y%m%dT%H%M%SZ").to_string()
}
fn date(value: &NaiveDate) -> String {
    value.format("%Y%m%d").to_string()
}

/// Renders a complete VCALENDAR. An empty event list is still a valid calendar.
pub(super) fn render(name: &str, stamp: &DateTime<Utc>, events: &[Event]) -> Option<String> {
    let mut out = String::new();
    let line = |out: &mut String, text: &str| fold_into(out, text);
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, "VERSION:2.0");
    line(&mut out, "PRODID:-//hrrdarr//Calendar Feed 1//EN");
    line(&mut out, "CALSCALE:GREGORIAN");
    line(&mut out, "METHOD:PUBLISH");
    line(&mut out, &format!("X-WR-CALNAME:{}", escape_text(name)));
    let stamp = utc(stamp);
    for event in events {
        line(&mut out, "BEGIN:VEVENT");
        line(&mut out, &format!("UID:{}", escape_text(&event.uid)));
        line(&mut out, &format!("DTSTAMP:{stamp}"));
        match &event.when {
            When::Timed { start, minutes } => {
                line(&mut out, &format!("DTSTART:{}", utc(start)));
                if let Some(minutes) = minutes.filter(|m| (1..=MAX_DURATION_MINUTES).contains(m)) {
                    line(&mut out, &format!("DURATION:PT{minutes}M"));
                }
            }
            When::AllDay(day) => {
                let next = day.succ_opt()?;
                line(&mut out, &format!("DTSTART;VALUE=DATE:{}", date(day)));
                line(&mut out, &format!("DTEND;VALUE=DATE:{}", date(&next)));
            }
        }
        line(
            &mut out,
            &format!("SUMMARY:{}", escape_text(&event.summary)),
        );
        if let Some(description) = &event.description {
            line(
                &mut out,
                &format!("DESCRIPTION:{}", escape_text(description)),
            );
        }
        if let Some(category) = &event.category {
            line(&mut out, &format!("CATEGORIES:{}", escape_text(category)));
        }
        line(
            &mut out,
            match event.status {
                Status::Confirmed => "STATUS:CONFIRMED",
                Status::Tentative => "STATUS:TENTATIVE",
            },
        );
        line(&mut out, "END:VEVENT");
    }
    line(&mut out, "END:VCALENDAR");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn stamp() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 18, 12, 30, 5).unwrap()
    }
    /// Independent unfolding (RFC 5545 section 3.1): CRLF followed by one space or tab.
    fn unfold(text: &str) -> Vec<String> {
        text.replace("\r\n ", "")
            .replace("\r\n\t", "")
            .split("\r\n")
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn escapes_text_specials_and_line_breaks() {
        assert_eq!(
            escape_text("a,b;c\\d\ne\r\nf\rg\th\u{7}i"),
            "a\\,b\\;c\\\\d\\ne\\nf\\ng\th i"
        );
        // An injected property line stays inside the escaped value.
        assert_eq!(
            escape_text("x\r\nEND:VEVENT"),
            "x\\nEND:VEVENT",
            "line breaks cannot start a new content line"
        );
    }

    #[test]
    fn folds_at_75_octets_without_splitting_utf8() {
        let mut out = String::new();
        let value = format!("SUMMARY:{}", "é".repeat(60));
        fold_into(&mut out, &value);
        assert!(out.ends_with("\r\n"));
        let physical: Vec<&str> = out.trim_end_matches("\r\n").split("\r\n").collect();
        assert!(physical.len() > 1);
        for (index, line) in physical.iter().enumerate() {
            assert!(
                line.len() <= MAX_LINE_OCTETS,
                "line {index} has {} octets",
                line.len()
            );
            if index > 0 {
                assert!(line.starts_with(' '));
            }
        }
        // Every physical line is valid UTF-8 by construction (&str); unfolding restores it.
        assert_eq!(unfold(&out)[0], value);
        let mut exact = String::new();
        fold_into(&mut exact, &"x".repeat(75));
        assert_eq!(exact, format!("{}\r\n", "x".repeat(75)));
        let mut over = String::new();
        fold_into(&mut over, &"x".repeat(76));
        assert_eq!(over, format!("{}\r\n x\r\n", "x".repeat(75)));
    }

    #[test]
    fn golden_feed_with_timed_and_all_day_events() {
        let events = vec![
            Event {
                uid: "episode-1@hrrdarr".into(),
                when: When::Timed {
                    start: Utc.with_ymd_and_hms(2026, 10, 20, 1, 0, 0).unwrap(),
                    minutes: Some(45),
                },
                summary: "Harbor, Inc; - 1x02 - Pilot".into(),
                description: Some("Line one\nLine two".into()),
                category: Some("Net".into()),
                status: Status::Tentative,
            },
            Event {
                uid: "movie-1-digital@hrrdarr".into(),
                when: When::AllDay(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
                summary: "Harbor - Digital release".into(),
                description: None,
                category: None,
                status: Status::Confirmed,
            },
            Event {
                uid: "episode-2@hrrdarr".into(),
                when: When::Timed {
                    start: Utc.with_ymd_and_hms(2026, 10, 21, 1, 0, 0).unwrap(),
                    minutes: None,
                },
                summary: "Harbor - Daily".into(),
                description: None,
                category: None,
                status: Status::Confirmed,
            },
        ];
        let text = render("hrrdarr TV", &stamp(), &events).unwrap();
        let expected = [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//hrrdarr//Calendar Feed 1//EN",
            "CALSCALE:GREGORIAN",
            "METHOD:PUBLISH",
            "X-WR-CALNAME:hrrdarr TV",
            "BEGIN:VEVENT",
            "UID:episode-1@hrrdarr",
            "DTSTAMP:20261018T123005Z",
            "DTSTART:20261020T010000Z",
            "DURATION:PT45M",
            "SUMMARY:Harbor\\, Inc\\; - 1x02 - Pilot",
            "DESCRIPTION:Line one\\nLine two",
            "CATEGORIES:Net",
            "STATUS:TENTATIVE",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "UID:movie-1-digital@hrrdarr",
            "DTSTAMP:20261018T123005Z",
            "DTSTART;VALUE=DATE:20261231",
            "DTEND;VALUE=DATE:20270101",
            "SUMMARY:Harbor - Digital release",
            "STATUS:CONFIRMED",
            "END:VEVENT",
            "BEGIN:VEVENT",
            "UID:episode-2@hrrdarr",
            "DTSTAMP:20261018T123005Z",
            "DTSTART:20261021T010000Z",
            "SUMMARY:Harbor - Daily",
            "STATUS:CONFIRMED",
            "END:VEVENT",
            "END:VCALENDAR",
            "",
        ]
        .join("\r\n");
        assert_eq!(text, expected);
        // No bare LF anywhere: every LF is preceded by CR.
        assert!(!text.replace("\r\n", "").contains('\n'));
        assert!(!text.replace("\r\n", "").contains('\r'));
    }

    #[test]
    fn empty_calendar_is_valid_and_durations_are_bounded() {
        let text = render("Empty", &stamp(), &[]).unwrap();
        assert!(text.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(text.ends_with("END:VCALENDAR\r\n"));
        assert!(!text.contains("VEVENT"));
        let long = Event {
            uid: "episode-9@hrrdarr".into(),
            when: When::Timed {
                start: stamp(),
                minutes: Some(MAX_DURATION_MINUTES + 1),
            },
            summary: "Long".into(),
            description: None,
            category: None,
            status: Status::Confirmed,
        };
        let text = render("Long", &stamp(), &[long]).unwrap();
        assert!(!text.contains("DURATION"));
    }

    #[test]
    fn long_multibyte_description_round_trips_through_unfolding() {
        let description = "Ünïcödé 🎬 overview, with; specials\\ and\nbreaks ".repeat(8);
        let event = Event {
            uid: "episode-3@hrrdarr".into(),
            when: When::AllDay(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
            summary: "Leap".into(),
            description: Some(description.clone()),
            category: None,
            status: Status::Confirmed,
        };
        let text = render("Leap", &stamp(), &[event]).unwrap();
        for physical in text.split("\r\n") {
            assert!(physical.len() <= MAX_LINE_OCTETS);
        }
        let lines = unfold(&text);
        assert!(lines.contains(&"DTEND;VALUE=DATE:20240301".to_string()));
        let line = lines
            .iter()
            .find(|l| l.starts_with("DESCRIPTION:"))
            .unwrap();
        assert_eq!(line, &format!("DESCRIPTION:{}", escape_text(&description)));
    }
}
