//! Versioned custom-format storage contract. Matching and regex validation belong to the evaluator.
use crate::api::MediaDomain;
use serde::{Deserialize, Serialize};

pub const SPECIFICATION_VERSION: i32 = 1;
pub const MAX_SPECIFICATIONS: usize = 64;
pub const MAX_SPECIFICATION_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CustomFormatSpecification")]
pub struct Specification {
    pub name: String,
    pub negate: bool,
    pub required: bool,
    pub condition: Condition,
}

// Numeric enum values retain their media domain; e.g. source 1 and flag 8 differ across domains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(rename = "CustomFormatCondition")]
pub enum Condition {
    ReleaseTitle { pattern: String },
    ReleaseGroup { pattern: String },
    Edition { pattern: String },
    Language { value: i32, except_language: bool },
    Size { min_gib: f64, max_gib: f64 },
    Source { value: i32 },
    Resolution { value: i32 },
    QualityModifier { value: i32 },
    IndexerFlag { value: i32 },
    ReleaseType { value: i32 },
    Year { min: i32, max: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "QualityProfileFormatScore")]
pub struct FormatScore {
    pub format_id: i64,
    pub score: i32,
}

/// Decode stored version 1 definitions with bounded allocation and domain-specific values.
/// Consumers must additionally compile patterns with their supported bounded regex engine.
pub fn decode_specifications(
    media: MediaDomain,
    version: i32,
    json: &str,
) -> Result<Vec<Specification>, &'static str> {
    if version != SPECIFICATION_VERSION || json.len() > MAX_SPECIFICATION_BYTES {
        return Err("unsupported or oversized custom format specifications");
    }
    let specs: Vec<Specification> =
        serde_json::from_str(json).map_err(|_| "invalid custom format specifications")?;
    if specs.is_empty() || specs.len() > MAX_SPECIFICATIONS {
        return Err("custom format must contain between 1 and 64 specifications");
    }
    let movies = matches!(media, MediaDomain::Movies);
    for spec in &specs {
        if spec.name.trim().is_empty()
            || spec.name.chars().count() > 100
            || spec.name.contains('\0')
        {
            return Err("invalid custom format specification name");
        }
        let valid = match &spec.condition {
            Condition::ReleaseTitle { pattern } | Condition::ReleaseGroup { pattern } => {
                valid_pattern_text(pattern)
            }
            Condition::Edition { pattern } => movies && valid_pattern_text(pattern),
            Condition::Language { value, .. } => {
                *value == -2
                    || (movies && *value == -1)
                    || (0..=if movies { 57 } else { 52 }).contains(value)
            }
            Condition::Size { min_gib, max_gib } => {
                min_gib.is_finite() && max_gib.is_finite() && *min_gib >= 0.0 && max_gib > min_gib
            }
            Condition::Source { value } => (0..=if movies { 9 } else { 7 }).contains(value),
            Condition::Resolution { value } => {
                matches!(value, 0 | 360 | 480 | 540 | 576 | 720 | 1080 | 2160)
            }
            Condition::QualityModifier { value } => movies && (0..=5).contains(value),
            Condition::IndexerFlag { value } => {
                *value > 0
                    && (*value as u32).is_power_of_two()
                    && *value <= if movies { 2048 } else { 256 }
            }
            Condition::ReleaseType { value } => !movies && (0..=3).contains(value),
            Condition::Year { min, max } => movies && *min > 0 && max >= min,
        };
        if !valid {
            return Err("custom format condition is invalid for this media domain");
        }
    }
    Ok(specs)
}

fn valid_pattern_text(pattern: &str) -> bool {
    !pattern.is_empty() && pattern.len() <= 4096 && !pattern.contains('\0')
}
