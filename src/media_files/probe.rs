//! Bounded ffprobe observations. This module neither opens media nor decides whether
//! it is importable. Missing measurements remain distinct from reported zero.
use serde_json::{Map, Value};

const MAX_RAW_BYTES: usize = 256 * 1024;
const MAX_STREAMS: usize = 64;
const MAX_TEXT_BYTES: usize = 1024;
const MAX_NORMALIZED_BYTES: usize = 64 * 1024;
const TICKS_PER_SECOND: i64 = 10_000_000;
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Debug)]
pub(crate) struct ProbedMedia {
    pub runtime_ticks: Option<i64>,
    /// Available only after a complete, typed stream inventory was parsed.
    pub audio_stream_count: usize,
    pub video_width: Option<i64>,
    pub video_height: Option<i64>,
    common: Map<String, Value>,
    audio: Vec<Value>,
    subtitles: Vec<Value>,
}
impl ProbedMedia {
    /// Existing stored DTO validation is authoritative. TV retains individual audio
    /// and subtitle facts; the existing movie DTO exposes first-audio/all-language
    /// summaries and cannot retain individual subtitle dispositions.
    pub fn normalized_info(&self, media: &str) -> Result<String> {
        if !matches!(media, "tv" | "movies") {
            return Err("probe_media_invalid");
        }
        let mut result = self.common.clone();
        if media == "tv" {
            result.insert("audioStreams".into(), Value::Array(self.audio.clone()));
            result.insert(
                "subtitleStreams".into(),
                Value::Array(self.subtitles.clone()),
            );
        } else {
            result.insert("audioStreamCount".into(), self.audio_stream_count.into());
            if let Some(first) = self.audio.first().and_then(Value::as_object) {
                for (from, to) in [
                    ("format", "audioFormat"),
                    ("codecId", "audioCodecID"),
                    ("profile", "audioProfile"),
                    ("bitrate", "audioBitrate"),
                    ("channels", "audioChannels"),
                    ("channelPositions", "audioChannelPositions"),
                ] {
                    if let Some(value) = first.get(from) {
                        result.insert(to.into(), value.clone());
                    }
                }
            }
            for (key, streams) in [
                ("audioLanguages", &self.audio),
                ("subtitles", &self.subtitles),
            ] {
                // One unknown language makes the combined language projection
                // unknown, rather than silently dropping that stream.
                let languages: Option<Vec<Value>> =
                    streams.iter().map(|s| s.get("language").cloned()).collect();
                if let Some(languages) = languages {
                    result.insert(key.into(), Value::Array(languages));
                }
            }
        }
        let raw = serde_json::to_string(&result).map_err(|_| "probe_normalization_invalid")?;
        if raw.len() > MAX_NORMALIZED_BYTES {
            return Err("probe_normalization_limit");
        }
        let (normalized, unsupported) =
            super::media_info::normalize(&raw, media).map_err(|_| "probe_normalization_invalid")?;
        if unsupported {
            return Err("probe_normalization_invalid");
        }
        normalized.ok_or("probe_normalization_invalid")
    }
}
fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().ok_or("probe_object_invalid")
}
fn present<'a>(value: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    value.get(name).filter(|v| !v.is_null())
}
fn text<'a>(value: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>> {
    present(value, name)
        .map(|v| {
            v.as_str()
                .filter(|s| !s.chars().any(char::is_control))
                .ok_or("probe_text_invalid")
        })
        .transpose()
}
fn measured_text<'a>(value: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>> {
    Ok(text(value, name)?.filter(|s| !s.is_empty() && *s != "N/A" && *s != "unknown"))
}
fn integer(value: &Map<String, Value>, name: &str, max: i64) -> Result<Option<i64>> {
    let Some(raw) = present(value, name) else {
        return Ok(None);
    };
    let number = if let Some(raw) = raw.as_str() {
        if raw == "N/A" {
            return Ok(None);
        }
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err("probe_integer_invalid");
        }
        raw.parse::<i64>().map_err(|_| "probe_integer_invalid")?
    } else {
        raw.as_i64().ok_or("probe_integer_invalid")?
    };
    if !(0..=max).contains(&number) {
        return Err("probe_integer_invalid");
    }
    Ok(Some(number))
}
fn put_text(to: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        to.insert(key.into(), value.into());
    }
}
fn put_integer(to: &mut Map<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        to.insert(key.into(), value.into());
    }
}
/// ffprobe reports decimal seconds, normally with six fractional digits. Conversion
/// is integer-only: sub-tick precision is truncated, never rounded above observation.
fn duration(value: &Value) -> Result<Option<i64>> {
    let owned;
    let raw = if let Some(s) = value.as_str() {
        s
    } else if value.is_number() {
        owned = value.to_string();
        &owned
    } else {
        return Err("probe_duration_invalid");
    };
    if raw == "N/A" {
        return Ok(None);
    }
    if raw.len() > 40 {
        return Err("probe_duration_invalid");
    }
    let (seconds, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if seconds.is_empty()
        || !seconds.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || raw.ends_with('.')
        || fraction.len() > 18
    {
        return Err("probe_duration_invalid");
    }
    let seconds: i64 = seconds.parse().map_err(|_| "probe_duration_invalid")?;
    let significant = &fraction[..fraction.len().min(7)];
    let subticks = if significant.is_empty() {
        0
    } else {
        significant
            .parse::<i64>()
            .map_err(|_| "probe_duration_invalid")?
            * 10_i64.pow((7 - significant.len()) as u32)
    };
    seconds
        .checked_mul(TICKS_PER_SECOND)
        .and_then(|s| s.checked_add(subticks))
        .map(Some)
        .ok_or("probe_duration_invalid")
}
fn duration_text(ticks: i64) -> String {
    let seconds = ticks / TICKS_PER_SECOND;
    let clock = format!(
        "{:02}:{:02}:{:02}.{:07}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        ticks % TICKS_PER_SECOND
    );
    if seconds >= 86400 {
        format!("{}.{}", seconds / 86400, clock)
    } else {
        clock
    }
}
fn frame_rate(value: &Map<String, Value>, name: &str) -> Result<Option<f64>> {
    let Some(raw) = present(value, name) else {
        return Ok(None);
    };
    if raw.as_str() == Some("N/A") {
        return Ok(None);
    }
    let rate = if let Some(raw) = raw.as_str() {
        if let Some((numerator, denominator)) = raw.split_once('/') {
            let parse = |s: &str| -> Result<i64> {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("probe_frame_rate_invalid");
                }
                s.parse().map_err(|_| "probe_frame_rate_invalid")
            };
            let numerator = parse(numerator)?;
            let denominator = parse(denominator)?;
            if denominator == 0 {
                return if numerator == 0 {
                    Ok(None)
                } else {
                    Err("probe_frame_rate_invalid")
                };
            }
            numerator as f64 / denominator as f64
        } else {
            raw.parse::<f64>().map_err(|_| "probe_frame_rate_invalid")?
        }
    } else {
        raw.as_f64().ok_or("probe_frame_rate_invalid")?
    };
    if !rate.is_finite() || !(0.0..=f64::from(i32::MAX)).contains(&rate) {
        return Err("probe_frame_rate_invalid");
    }
    // ffprobe's zero numerator denotes an unknown frame rate, unlike a
    // measured zero duration/audio count. Permit the alternate rate to supply it.
    Ok((rate > 0.0).then_some(rate))
}
fn disposition(stream: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    let Some(disposition) = present(stream, "disposition") else {
        return Ok(None);
    };
    let o = object(disposition)?;
    integer(o, key, 1).map(|v| v.map(|v| v == 1))
}
fn language(stream: &Map<String, Value>) -> Result<Option<&str>> {
    let Some(tags) = present(stream, "tags") else {
        return Ok(None);
    };
    // ISO 'und' reports an undetermined language; it is not a measured language.
    Ok(measured_text(object(tags)?, "language")?.filter(|s| *s != "und"))
}
/// Bound even ignored metadata (including side-data) before projection. The JSON
/// decoder's recursion limit is retained, with a stricter semantic depth/node limit.
fn validate_tree(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if depth > 16 || *nodes > 16_384 {
        return Err("probe_metadata_limit");
    }
    match value {
        Value::String(s)
            if s.len() > MAX_TEXT_BYTES
                || s.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
        {
            return Err("probe_text_invalid");
        }
        Value::Object(o) => {
            for (key, value) in o {
                if key.len() > MAX_TEXT_BYTES || key.chars().any(char::is_control) {
                    return Err("probe_text_invalid");
                }
                validate_tree(value, depth + 1, nodes)?;
            }
        }
        Value::Array(a) => {
            for value in a {
                validate_tree(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn parse(raw: &[u8]) -> Result<ProbedMedia> {
    if raw.len() > MAX_RAW_BYTES {
        return Err("probe_output_limit");
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| "probe_json_invalid")?;
    validate_tree(&value, 0, &mut 0)?;
    let root = object(&value)?;
    if present(root, "error").is_some() {
        return Err("probe_reported_error");
    }
    // Absence is a failed inventory, never a fabricated zero-audio measurement.
    let streams = root
        .get("streams")
        .ok_or("probe_streams_missing")?
        .as_array()
        .ok_or("probe_streams_invalid")?;
    if streams.len() > MAX_STREAMS {
        return Err("probe_stream_limit");
    }
    let format = present(root, "format").map(object).transpose()?;
    let runtime_ticks = format
        .and_then(|f| present(f, "duration"))
        .map(duration)
        .transpose()?
        .flatten();
    let mut common = Map::new();
    if let Some(format) = format {
        put_text(
            &mut common,
            "containerFormat",
            measured_text(format, "format_name")?,
        );
        // Size is descriptor-owned in the import inventory, not part of MediaInfo.
        // If a broader runner requests it, reject invalid numeric observations.
        integer(format, "size", i64::MAX)?;
        integer(format, "bit_rate", i64::MAX)?;
    }
    if let Some(ticks) = runtime_ticks {
        common.insert("runTime".into(), duration_text(ticks).into());
    }
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    let mut video_seen = false;
    let mut video_width = None;
    let mut video_height = None;
    for stream in streams {
        let stream = object(stream)?;
        let kind = text(stream, "codec_type")?.ok_or("probe_stream_type_missing")?;
        match kind {
            "video" => {
                let width = integer(stream, "width", i32::MAX.into())?;
                let height = integer(stream, "height", i32::MAX.into())?;
                let bitrate = integer(stream, "bit_rate", i64::MAX)?;
                let bit_depth = integer(stream, "bits_per_raw_sample", i32::MAX.into())?;
                let average = frame_rate(stream, "avg_frame_rate")?;
                let regular = frame_rate(stream, "r_frame_rate")?;
                let attached = disposition(stream, "attached_pic")?.unwrap_or(false);
                // Cover-art dimensions are never the movie/episode video dimensions.
                if attached || video_seen {
                    continue;
                }
                video_seen = true;
                video_width = width;
                video_height = height;
                put_integer(&mut common, "width", width);
                put_integer(&mut common, "height", height);
                put_integer(&mut common, "videoBitrate", bitrate);
                put_integer(&mut common, "videoBitDepth", bit_depth);
                put_text(
                    &mut common,
                    "videoFormat",
                    measured_text(stream, "codec_name")?,
                );
                put_text(
                    &mut common,
                    "videoCodecID",
                    measured_text(stream, "codec_tag_string")?,
                );
                put_text(
                    &mut common,
                    "videoProfile",
                    measured_text(stream, "profile")?,
                );
                if let Some(rate) = average.or(regular) {
                    common.insert("videoFps".into(), rate.into());
                }
                let scan_type = match measured_text(stream, "field_order")? {
                    Some("progressive") => Some("Progressive"),
                    Some("tt" | "bb" | "tb" | "bt") => Some("Interlaced"),
                    _ => None,
                };
                put_text(&mut common, "scanType", scan_type);
                // Color transfer/primaries and side-data are not interchangeable
                // with an HDR profile. Until a complete supported profile is proven,
                // leave HDR unknown rather than infer HDR10/DV from a single tag.
            }
            "audio" => {
                let mut item = Map::new();
                put_text(&mut item, "language", language(stream)?);
                put_text(&mut item, "format", measured_text(stream, "codec_name")?);
                put_text(
                    &mut item,
                    "codecId",
                    measured_text(stream, "codec_tag_string")?,
                );
                put_text(&mut item, "profile", measured_text(stream, "profile")?);
                put_integer(&mut item, "bitrate", integer(stream, "bit_rate", i64::MAX)?);
                put_integer(
                    &mut item,
                    "channels",
                    integer(stream, "channels", i32::MAX.into())?,
                );
                put_text(
                    &mut item,
                    "channelPositions",
                    measured_text(stream, "channel_layout")?,
                );
                audio.push(Value::Object(item));
            }
            "subtitle" => {
                let mut item = Map::new();
                put_text(&mut item, "language", language(stream)?);
                put_text(&mut item, "format", measured_text(stream, "codec_name")?);
                for (from, to) in [
                    ("forced", "forced"),
                    ("hearing_impaired", "hearingImpaired"),
                ] {
                    if let Some(flag) = disposition(stream, from)? {
                        item.insert(to.into(), flag.into());
                    }
                }
                subtitles.push(Value::Object(item));
            }
            "data" | "attachment" | "unknown" => {}
            _ => return Err("probe_stream_type_invalid"),
        }
    }
    Ok(ProbedMedia {
        runtime_ticks,
        audio_stream_count: audio.len(),
        video_width,
        video_height,
        common,
        audio,
        subtitles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn probe(value: Value) -> ProbedMedia {
        parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }
    fn info(value: &ProbedMedia, media: &str) -> Value {
        serde_json::from_str(&value.normalized_info(media).unwrap()).unwrap()
    }
    #[test]
    fn stream_inventory_absence_is_not_measured_zero() {
        for raw in [
            br#"{}"#.as_slice(),
            br#"{"streams":null}"#,
            br#"{"streams":{}}"#,
            br#"{"streams":[{}]}"#,
            br#"{"streams":[{"codec_type":3}]}"#,
        ] {
            assert!(parse(raw).is_err());
        }
        let missing = probe(json!({"streams":[]}));
        assert_eq!(missing.audio_stream_count, 0);
        assert_eq!(missing.runtime_ticks, None);
        for media in ["tv", "movies"] {
            let output = info(&missing, media);
            assert_eq!(output["audio_stream_count"], 0);
            assert!(output["runtime_ticks"].is_null());
        }
        let zero = probe(json!({"streams":[],"format":{"duration":"0.000000"}}));
        assert_eq!(zero.runtime_ticks, Some(0));
        assert_eq!(info(&zero, "tv")["runtime_ticks"], 0);
        assert_eq!(
            probe(json!({"streams":[],"format":{"duration":"N/A"}})).runtime_ticks,
            None
        );
        assert!(parse(br#"{"streams":[],"error":{"code":-1,"string":"failed"}}"#).is_err());
    }
    #[test]
    fn duration_is_exact_bounded_and_not_a_wall_clock() {
        for (raw, ticks) in [
            ("0", 0),
            ("0.0000001", 1),
            ("0.00000019", 1),
            ("90061.1234567", 900611234567),
            ("922337203685.4775807", i64::MAX),
        ] {
            let p = probe(json!({"streams":[],"format":{"duration":raw}}));
            assert_eq!(p.runtime_ticks, Some(ticks));
            for media in ["tv", "movies"] {
                assert_eq!(info(&p, media)["runtime_ticks"], ticks);
            }
        }
        for raw in [
            "-1",
            "NaN",
            "inf",
            "Infinity",
            "1e309",
            "922337203685.4775808",
            "99999999999999999999999999",
            "1.",
            ".1",
            "1.2.3",
        ] {
            assert!(
                parse(
                    &serde_json::to_vec(&json!({"streams":[],"format":{"duration":raw}})).unwrap()
                )
                .is_err(),
                "{raw}"
            );
        }
        assert_eq!(
            probe(json!({"streams":[],"format":{"duration":1.5}})).runtime_ticks,
            Some(15_000_000)
        );
    }
    #[test]
    fn real_stream_projection_preserves_both_domain_facts() {
        let p = probe(
            json!({"format":{"format_name":"matroska,webm","duration":"90123.125000","size":"1073741824"},"streams":[
                {"codec_type":"video","codec_name":"mjpeg","width":600,"height":600,"disposition":{"attached_pic":1}},
                {"codec_type":"video","codec_name":"hevc","codec_tag_string":"hvc1","profile":"Main 10","width":3840,"height":2160,"bit_rate":"12000000","bits_per_raw_sample":"10","avg_frame_rate":"24000/1001","r_frame_rate":"24/1","field_order":"progressive","color_transfer":"smpte2084","color_primaries":"bt2020","side_data_list":[{"side_data_type":"Display Matrix","displaymatrix":"\n00000000: 65536 0 0\n00000001: 0 65536 0"}],"disposition":{"attached_pic":0}},
                {"codec_type":"audio","codec_name":"eac3","codec_tag_string":"ec-3","profile":"Dolby Digital Plus","bit_rate":"640000","channels":6,"channel_layout":"5.1(side)","tags":{"language":"eng"}},
                {"codec_type":"audio","codec_name":"aac","channels":2,"channel_layout":"stereo","tags":{"language":"isl"}},
                {"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"eng"},"disposition":{"forced":1,"hearing_impaired":0}},
                {"codec_type":"attachment","codec_name":"ttf"}
            ]}),
        );
        assert_eq!((p.video_width, p.video_height), (Some(3840), Some(2160)));
        assert_eq!(p.audio_stream_count, 2);
        for media in ["tv", "movies"] {
            let v = info(&p, media);
            assert_eq!(v["video_format"], "hevc");
            assert_eq!(v["video_codec_id"], "hvc1");
            assert_eq!(v["video_profile"], "Main 10");
            assert_eq!(v["video_bitrate"], 12_000_000);
            assert_eq!(v["video_bit_depth"], 10);
            assert_eq!(v["video_fps"], 23.976);
            assert_eq!(v["audio_stream_count"], 2);
            assert_eq!(v["audio_channels"], 5.1);
            assert_eq!(v["audio_channel_count"], 6);
            assert_eq!(v["audio_languages"], "eng/isl");
            assert_eq!(v["audio_format"], "eac3");
            assert_eq!(v["audio_bitrate"], 640000);
            assert_eq!(v["resolution"], "3840x2160");
            assert_eq!(v["run_time"], "25:02:03");
            assert!(v["video_dynamic_range"].is_null());
            assert!(v["video_dynamic_range_type"].is_null());
            assert_eq!(v["subtitles"], "eng");
        }
        let tv = info(&p, "tv");
        assert_eq!(tv["subtitle_streams"][0]["forced"], true);
        assert_eq!(tv["subtitle_streams"][0]["hearing_impaired"], false);
        assert_eq!(tv["audio_streams"][1]["format"], "aac");
        assert!(info(&p, "movies")["subtitle_streams"].is_null());
        assert!(p.normalized_info("movie").is_err());
    }
    #[test]
    fn absent_languages_and_attached_pictures_do_not_invent_facts() {
        let p = probe(
            json!({"streams":[{"codec_type":"video","width":512,"height":512,"disposition":{"attached_pic":1}},{"codec_type":"audio","tags":{"language":"und"}},{"codec_type":"subtitle"}]}),
        );
        assert_eq!(p.audio_stream_count, 1);
        assert_eq!((p.video_width, p.video_height), (None, None));
        for media in ["tv", "movies"] {
            let v = info(&p, media);
            assert!(v["audio_languages"].is_null());
            assert!(v["subtitles"].is_null());
            assert!(v["audio_channel_count"].is_null());
            assert!(v["video_format"].is_null());
        }
        let no_audio =
            probe(json!({"streams":[{"codec_type":"video","width":1920,"height":1080}]}));
        assert_eq!(no_audio.audio_stream_count, 0);
    }
    #[test]
    fn rational_rates_and_numeric_fields_fail_closed() {
        for (average, regular, expected) in [
            ("0/0", "30000/1001", Some(29.97)),
            ("0/1", "25/1", Some(25.0)),
            ("N/A", "0/0", None),
            ("0/1", "0/1", None),
        ] {
            let p = probe(
                json!({"streams":[{"codec_type":"video","avg_frame_rate":average,"r_frame_rate":regular}]}),
            );
            assert_eq!(info(&p, "tv")["video_fps"].as_f64(), expected);
        }
        for (field, value) in [
            ("avg_frame_rate", json!("1/0")),
            ("avg_frame_rate", json!("-1/2")),
            ("avg_frame_rate", json!("NaN")),
            ("avg_frame_rate", json!("Infinity")),
            ("avg_frame_rate", json!("9223372036854775808/1")),
            ("width", json!(-1)),
            ("width", json!(2147483648_i64)),
            ("bit_rate", json!("9223372036854775808")),
            ("bits_per_raw_sample", json!(-1)),
        ] {
            let mut stream = json!({"codec_type":"video"});
            stream[field] = value;
            assert!(
                parse(&serde_json::to_vec(&json!({"streams":[stream]})).unwrap()).is_err(),
                "{field}"
            );
        }
        for stream in [
            json!({"codec_type":"audio","channels":-1}),
            json!({"codec_type":"audio","tags":{"language":2}}),
            json!({"codec_type":"subtitle","disposition":{"forced":2}}),
        ] {
            assert!(parse(&serde_json::to_vec(&json!({"streams":[stream]})).unwrap()).is_err());
        }
    }
    #[test]
    fn raw_stream_text_and_normalized_bounds_are_enforced() {
        assert_eq!(
            parse(&vec![b' '; MAX_RAW_BYTES + 1]).unwrap_err(),
            "probe_output_limit"
        );
        assert!(parse(b"{").is_err());
        assert!(parse(b"[]").is_err());
        assert!(parse(b"\xff").is_err());
        let stream = json!({"codec_type":"audio"});
        assert_eq!(
            parse(&serde_json::to_vec(&json!({"streams":vec![stream.clone();65]})).unwrap())
                .unwrap_err(),
            "probe_stream_limit"
        );
        assert_eq!(
            probe(json!({"streams":vec![stream;64]})).audio_stream_count,
            64
        );
        assert!(
            parse(&serde_json::to_vec(&json!({"streams":[],"extra":"x".repeat(1025)})).unwrap())
                .is_err()
        );
        assert!(
            parse(
                &serde_json::to_vec(
                    &json!({"streams":[{"codec_type":"audio","codec_name":"bad\nlabel"}]})
                )
                .unwrap()
            )
            .is_err()
        );
        let mut nested = json!(0);
        for _ in 0..18 {
            nested = json!([nested]);
        }
        assert!(
            parse(&serde_json::to_vec(&json!({"streams":[],"extra":nested})).unwrap()).is_err()
        );
        // Valid individual texts can still exceed the existing stored DTO budget.
        let stream =
            json!({"codec_type":"audio","codec_name":"x".repeat(1024),"profile":"p".repeat(1024)});
        let p = probe(json!({"streams":vec![stream;64]}));
        assert!(p.normalized_info("tv").is_err());
    }
}
