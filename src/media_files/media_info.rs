//! Independently normalized scan facts. Source raw probes and unknown fields stay private.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
const MAX_JSON_BYTES: usize = 64 * 1024;
const MAX_STREAMS: usize = 64;
const MAX_TEXT_BYTES: usize = 1024;
const TICKS_PER_SECOND: i64 = 10_000_000;
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioStream {
    language: Option<String>,
    format: Option<String>,
    codec_id: Option<String>,
    profile: Option<String>,
    bitrate: Option<i64>,
    channels: Option<i64>,
    channel_positions: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubtitleStream {
    language: Option<String>,
    format: Option<String>,
    forced: Option<bool>,
    hearing_impaired: Option<bool>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaInfo {
    schema_revision: Option<i64>,
    container_format: Option<String>,
    audio_bitrate: Option<i64>,
    audio_channels: Option<f64>,
    audio_codec: Option<String>,
    audio_languages: Option<String>,
    audio_stream_count: Option<i64>,
    video_bit_depth: Option<i64>,
    video_bitrate: Option<i64>,
    video_codec: Option<String>,
    video_fps: Option<f64>,
    video_dynamic_range: Option<String>,
    video_dynamic_range_type: Option<String>,
    resolution: Option<String>,
    run_time: Option<String>,
    scan_type: Option<String>,
    subtitles: Option<String>,
    // Native factual fields are deliberately distinct from formatted codec labels above.
    video_format: Option<String>,
    video_codec_id: Option<String>,
    video_profile: Option<String>,
    audio_format: Option<String>,
    audio_codec_id: Option<String>,
    audio_profile: Option<String>,
    audio_channel_count: Option<i64>,
    audio_channel_positions: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    runtime_ticks: Option<i64>,
    audio_streams: Option<Vec<AudioStream>>,
    subtitle_streams: Option<Vec<SubtitleStream>>,
}
fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().ok_or("Invalid media-info object")
}
fn present<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    o.get(key).filter(|v| !v.is_null())
}
fn text(o: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    present(o, key)
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= MAX_TEXT_BYTES && !s.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or("Invalid media-info text")
        })
        .transpose()
}
fn integer(o: &Map<String, Value>, key: &str, max: i64) -> Result<Option<i64>> {
    present(o, key)
        .map(|v| {
            v.as_i64()
                .filter(|v| *v >= 0 && *v <= max)
                .ok_or("Invalid media-info integer")
        })
        .transpose()
}
fn boolean(o: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    present(o, key)
        .map(|v| v.as_bool().ok_or("Invalid media-info boolean"))
        .transpose()
}
fn extras(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.keys().any(|k| !keys.contains(&k.as_str()))
}
fn array<'a>(o: &'a Map<String, Value>, key: &str) -> Result<Option<&'a Vec<Value>>> {
    present(o, key)
        .map(|v| {
            v.as_array()
                .filter(|a| a.len() <= MAX_STREAMS)
                .ok_or("Media-info stream array exceeds bounds or is invalid")
        })
        .transpose()
}
fn strings(o: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    array(o, key)?
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| s.len() <= MAX_TEXT_BYTES && !s.chars().any(char::is_control))
                        .ok_or("Invalid media-info language")
                })
                .collect::<Result<Vec<_>>>()
                .map(|v| v.join("/"))
        })
        .transpose()
}
// .NET constant TimeSpan: [d.]hh:mm:ss[.fffffff]. Keep exact 100 ns ticks;
// durations are never interpreted as wall-clock timestamps or reduced modulo one day.
fn duration(raw: &str) -> Result<i64> {
    if raw.len() > 32 || !raw.is_ascii() {
        return Err("Invalid media-info duration");
    }
    let parts: Vec<_> = raw.split(':').collect();
    if parts.len() != 3 {
        return Err("Invalid media-info duration");
    }
    let (days, hours) = match parts[0].split_once('.') {
        Some((d, h)) => (parse_digits(d)?, h),
        None => (0, parts[0]),
    };
    let (seconds, fraction) = parts[2].split_once('.').unwrap_or((parts[2], ""));
    if hours.len() != 2
        || parts[1].len() != 2
        || seconds.len() != 2
        || fraction.len() > 7
        || parts[2].ends_with('.')
    {
        return Err("Invalid media-info duration");
    }
    let hour = parse_digits(hours)?;
    let minute = parse_digits(parts[1])?;
    let second = parse_digits(seconds)?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err("Invalid media-info duration");
    }
    let fraction = if fraction.is_empty() {
        0
    } else {
        parse_digits(fraction)?
            .checked_mul(10_i64.pow((7 - fraction.len()) as u32))
            .ok_or("Media-info duration overflow")?
    };
    days.checked_mul(86400)
        .and_then(|v| v.checked_add(hour * 3600 + minute * 60 + second))
        .and_then(|v| v.checked_mul(TICKS_PER_SECOND))
        .and_then(|v| v.checked_add(fraction))
        .ok_or("Media-info duration overflow")
}
fn parse_digits(s: &str) -> Result<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid media-info duration");
    }
    s.parse().map_err(|_| "Media-info duration overflow")
}
fn runtime(ticks: i64) -> String {
    let seconds = ticks / TICKS_PER_SECOND;
    let hours = seconds / 3600;
    if hours > 0 {
        format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
fn hdr(value: Option<&Value>, media: &str) -> Result<(Option<String>, Option<String>, bool)> {
    let Some(value) = value else {
        return Ok((None, None, false));
    };
    // The catalogs differ: Sonarr has UnknownHdr at1; Radarr has Pq10 at1.
    let tv = [
        "none",
        "unknownHdr",
        "pq10",
        "hdr10",
        "hdr10Plus",
        "hlg10",
        "dolbyVision",
        "dolbyVisionHdr10",
        "dolbyVisionSdr",
        "dolbyVisionHlg",
        "dolbyVisionHdr10Plus",
    ];
    let movies = [
        "none",
        "pq10",
        "hdr10",
        "hdr10Plus",
        "hlg10",
        "dolbyVision",
        "dolbyVisionHdr10",
        "dolbyVisionSdr",
        "dolbyVisionHlg",
        "dolbyVisionHdr10Plus",
    ];
    let known: &[&str] = if media == "tv" { &tv } else { &movies };
    let name = if let Some(id) = value.as_i64() {
        usize::try_from(id)
            .ok()
            .and_then(|id| known.get(id).copied())
    } else if let Some(name) = value.as_str() {
        known.iter().copied().find(|v| *v == name)
    } else {
        return Err("Invalid media-info HDR enum");
    };
    let Some(name) = name else {
        return Ok((None, None, true));
    };
    let label = match name {
        "none" => "",
        "unknownHdr" => "HDR",
        "pq10" => "PQ",
        "hdr10" => "HDR10",
        "hdr10Plus" => "HDR10Plus",
        "hlg10" => "HLG",
        "dolbyVision" => "DV",
        "dolbyVisionHdr10" => "DV HDR10",
        "dolbyVisionSdr" => "DV SDR",
        "dolbyVisionHlg" => "DV HLG",
        "dolbyVisionHdr10Plus" => "DV HDR10Plus",
        _ => unreachable!(),
    };
    Ok((
        Some(if name == "none" { "" } else { "HDR" }.into()),
        Some(label.into()),
        false,
    ))
}

fn channels(layout: Option<&str>, count: Option<i64>) -> Option<f64> {
    // A numeric speaker-layout label is different from a count of discrete channels.
    // Only the source's one-digit x.y prefix convention is interpreted here.
    let prefix = layout
        .map(str::as_bytes)
        .filter(|b| b.len() >= 3)
        .filter(|b| b[0].is_ascii_digit() && b[1] == b'.' && b[2].is_ascii_digit());
    let named = prefix.map(|b| f64::from(b[0] - b'0') + f64::from(b[2] - b'0') / 10.0);
    named
        .filter(|v| *v > 0.0)
        .or_else(|| count.map(|v| v as f64))
}

pub(crate) fn normalize(raw: &str, media: &str) -> Result<(Option<String>, bool)> {
    if raw.len() > MAX_JSON_BYTES {
        return Err("Media-info JSON exceeds 64 KiB");
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "Invalid media-info JSON")?;
    if value.is_null() {
        return Ok((None, false));
    }
    let o = object(&value)?;
    let revision = integer(o, "schemaRevision", i32::MAX as i64)?;
    if revision.is_some_and(|v| v > 14) {
        return Ok((None, true));
    }
    let common = [
        "schemaRevision",
        "containerFormat",
        "videoFormat",
        "videoCodecID",
        "videoProfile",
        "videoBitrate",
        "videoBitDepth",
        "height",
        "width",
        "runTime",
        "videoFps",
        "videoHdrFormat",
        "scanType",
    ];
    let audio = if media == "tv" {
        vec!["audioStreams", "subtitleStreams"]
    } else {
        vec![
            "audioFormat",
            "audioCodecID",
            "audioProfile",
            "audioBitrate",
            "audioStreamCount",
            "audioChannels",
            "audioChannelPositions",
            "audioLanguages",
            "subtitles",
        ]
    };
    let mut known = common.to_vec();
    known.extend(audio);
    let mut unsupported = extras(o, &known);
    let mut info = MediaInfo {
        schema_revision: revision,
        container_format: text(o, "containerFormat")?,
        video_format: text(o, "videoFormat")?,
        video_codec_id: text(o, "videoCodecID")?,
        video_profile: text(o, "videoProfile")?,
        video_bitrate: integer(o, "videoBitrate", i64::MAX)?,
        video_bit_depth: integer(o, "videoBitDepth", i32::MAX as i64)?,
        width: integer(o, "width", i32::MAX as i64)?,
        height: integer(o, "height", i32::MAX as i64)?,
        scan_type: text(o, "scanType")?,
        ..Default::default()
    };
    info.resolution = info.width.zip(info.height).map(|(w, h)| format!("{w}x{h}"));
    info.video_fps = present(o, "videoFps")
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0 && *v <= i32::MAX as f64)
                .map(|v| (v * 1000.0).round_ties_even() / 1000.0)
                .ok_or("Invalid media-info frame rate")
        })
        .transpose()?;
    info.runtime_ticks = text(o, "runTime")?.map(|v| duration(&v)).transpose()?;
    info.run_time = info.runtime_ticks.map(runtime);
    let (dynamic, kind, unknown) = hdr(present(o, "videoHdrFormat"), media)?;
    info.video_dynamic_range = dynamic;
    info.video_dynamic_range_type = kind;
    unsupported |= unknown;
    if media == "tv" {
        if let Some(streams) = array(o, "audioStreams")? {
            let mut mapped = vec![];
            for stream in streams {
                let a = object(stream)?;
                unsupported |= extras(
                    a,
                    &[
                        "language",
                        "format",
                        "codecId",
                        "profile",
                        "bitrate",
                        "channels",
                        "channelPositions",
                    ],
                );
                mapped.push(AudioStream {
                    language: text(a, "language")?,
                    format: text(a, "format")?,
                    codec_id: text(a, "codecId")?,
                    profile: text(a, "profile")?,
                    bitrate: integer(a, "bitrate", i64::MAX)?,
                    channels: integer(a, "channels", i32::MAX as i64)?,
                    channel_positions: text(a, "channelPositions")?,
                });
            }
            info.audio_stream_count = Some(mapped.len() as i64);
            // Missing languages remain unknown rather than being silently omitted from the projection.
            info.audio_languages = mapped
                .iter()
                .map(|s| s.language.as_deref())
                .collect::<Option<Vec<_>>>()
                .map(|v| v.join("/"));
            if let Some(first) = mapped.first() {
                info.audio_bitrate = first.bitrate;
                info.audio_channel_count = first.channels;
                info.audio_channel_positions = first.channel_positions.clone();
                info.audio_format = first.format.clone();
                info.audio_codec_id = first.codec_id.clone();
                info.audio_profile = first.profile.clone();
            }
            info.audio_streams = Some(mapped);
        }
        if let Some(streams) = array(o, "subtitleStreams")? {
            let mut mapped = vec![];
            for stream in streams {
                let s = object(stream)?;
                unsupported |= extras(s, &["language", "format", "forced", "hearingImpaired"]);
                mapped.push(SubtitleStream {
                    language: text(s, "language")?,
                    format: text(s, "format")?,
                    forced: boolean(s, "forced")?,
                    hearing_impaired: boolean(s, "hearingImpaired")?,
                });
            }
            info.subtitles = mapped
                .iter()
                .map(|s| s.language.as_deref())
                .collect::<Option<Vec<_>>>()
                .map(|v| v.join("/"));
            info.subtitle_streams = Some(mapped);
        }
    } else {
        info.audio_bitrate = integer(o, "audioBitrate", i64::MAX)?;
        info.audio_channel_count = integer(o, "audioChannels", i32::MAX as i64)?;
        info.audio_channel_positions = text(o, "audioChannelPositions")?;
        info.audio_format = text(o, "audioFormat")?;
        info.audio_codec_id = text(o, "audioCodecID")?;
        info.audio_profile = text(o, "audioProfile")?;
        info.audio_stream_count = integer(o, "audioStreamCount", i32::MAX as i64)?;
        info.audio_languages = strings(o, "audioLanguages")?;
        info.subtitles = strings(o, "subtitles")?;
    }
    info.audio_channels = channels(
        info.audio_channel_positions.as_deref(),
        info.audio_channel_count,
    );
    let normalized = serde_json::to_string(&info).map_err(|_| "Invalid normalized media info")?;
    if normalized.len() > MAX_JSON_BYTES {
        return Err("Normalized media info exceeds 64 KiB");
    }
    Ok((Some(normalized), unsupported))
}

pub(super) fn public(raw: Option<String>) -> Result<Value> {
    let Some(raw) = raw else {
        return Ok(Value::Null);
    };
    if raw.len() > MAX_JSON_BYTES {
        return Err("Invalid stored media info");
    }
    let info: MediaInfo = serde_json::from_str(&raw).map_err(|_| "Invalid stored media info")?;
    serde_json::to_value(info).map_err(|_| "Invalid stored media info")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn normalized(value: Value, media: &str) -> Value {
        let (raw, _) = normalize(&value.to_string(), media).unwrap();
        public(raw).unwrap()
    }
    #[test]
    fn exact_duration_layout_null_and_domain_hdr_contracts() {
        for (text, ticks) in [
            ("00:00:00", 0),
            ("00:00:00.0000001", 1),
            ("1.01:02:03.1234567", 901231234567),
            ("10675199.02:48:05.4775807", i64::MAX),
        ] {
            assert_eq!(duration(text).unwrap(), ticks);
        }
        for invalid in [
            "",
            "1",
            "1:02:03",
            "24:00:00",
            "00:60:00",
            "00:00:60",
            "-01:00:00",
            "00:00:00.",
            "00:00:00.12345678",
            "1.2.03:04:05",
            "10675199.02:48:05.4775808",
            "99999999999999.00:00:00",
            "00:00:00Z",
        ] {
            assert!(duration(invalid).is_err(), "{invalid}");
        }
        for (layout, count, expected) in [
            (Some("5.1(side)"), Some(6), Some(5.1)),
            (Some("7.1"), Some(8), Some(7.1)),
            (Some("0.0"), Some(6), Some(6.0)),
            (Some("stereo"), Some(2), Some(2.0)),
            (None, Some(0), Some(0.0)),
            (None, None, None),
        ] {
            assert_eq!(channels(layout, count), expected);
        }
        let missing = normalized(json!({}), "tv");
        assert!(missing["audio_stream_count"].is_null());
        assert!(missing["video_bit_depth"].is_null());
        assert!(missing["run_time"].is_null());
        let zero = normalized(
            json!({"audioStreams":[],"subtitleStreams":[],"videoBitrate":0,"videoBitDepth":0,"videoFps":0,"width":0,"height":0,"runTime":"00:00:00","videoHdrFormat":"none"}),
            "tv",
        );
        assert_eq!(zero["audio_stream_count"], 0);
        assert_eq!(zero["audio_languages"], "");
        assert_eq!(zero["subtitles"], "");
        assert_eq!(zero["video_fps"], 0.0);
        assert_eq!(zero["resolution"], "0x0");
        assert_eq!(zero["run_time"], "0:00");
        assert_eq!(zero["video_dynamic_range_type"], "");
        assert_eq!(
            normalized(json!({"videoHdrFormat":1}), "tv")["video_dynamic_range_type"],
            "HDR"
        );
        assert_eq!(
            normalized(json!({"videoHdrFormat":1}), "movies")["video_dynamic_range_type"],
            "PQ"
        );
        for media in ["tv", "movies"] {
            let (raw, unknown) = normalize(r#"{"videoHdrFormat":"futureHdr"}"#, media).unwrap();
            assert!(unknown);
            assert!(public(raw).unwrap()["video_dynamic_range_type"].is_null());
            let (raw, unknown) = normalize(r#"{"schemaRevision":15,"width":1920}"#, media).unwrap();
            assert!(unknown);
            assert!(raw.is_none());
            assert_eq!(
                normalized(json!({"runTime":"1.01:02:03"}), media)["run_time"],
                "25:02:03"
            );
        }
        // Exact three-place ties use the native half-even rule, preserving factual zero.
        assert_eq!(
            normalized(json!({"videoFps":23.9765}), "tv")["video_fps"],
            23.976
        );
        assert_eq!(
            normalized(json!({"videoFps":23.9775}), "tv")["video_fps"],
            23.978
        );
    }
}
