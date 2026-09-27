//! Snapshot storage contracts, independently adapted into the native validated model.
use super::*;
use crate::{
    api::MediaDomain,
    custom_formats as native,
    db::custom_formats::{Condition, Specification},
};
use serde_json::Value as J;

pub(super) struct Catalog {
    pub definitions: BTreeMap<i64, Option<(native::Input, String)>>,
    pub observed: bool,
}
fn object(v: &J) -> Option<BTreeMap<String, &J>> {
    let mut out = BTreeMap::new();
    for (k, v) in v.as_object()? {
        if out.insert(k.to_ascii_lowercase(), v).is_some() {
            return None;
        }
    }
    Some(out)
}
fn specification(v: &J) -> Option<Specification> {
    let wrapper = object(v)?;
    if wrapper.len() != 2 {
        return None;
    }
    let kind = wrapper.get("type")?.as_str()?;
    let body = object(wrapper.get("body")?)?;
    let extra: &[&str] = match kind {
        "LanguageSpecification" => &["value", "exceptlanguage"],
        "SizeSpecification" | "YearSpecification" => &["min", "max"],
        _ => &["value"],
    };
    if body.keys().any(|k| {
        ![
            "name",
            "negate",
            "required",
            "order",
            "implementationname",
            "infolink",
        ]
        .contains(&k.as_str())
            && !extra.contains(&k.as_str())
    }) {
        return None;
    }
    let integer = |key: &str| i32::try_from(body.get(key)?.as_i64()?).ok();
    let condition = match kind {
        "ReleaseTitleSpecification" => Condition::ReleaseTitle {
            pattern: body.get("value")?.as_str()?.into(),
        },
        "ReleaseGroupSpecification" => Condition::ReleaseGroup {
            pattern: body.get("value")?.as_str()?.into(),
        },
        "EditionSpecification" => Condition::Edition {
            pattern: body.get("value")?.as_str()?.into(),
        },
        "LanguageSpecification" => Condition::Language {
            value: integer("value")?,
            except_language: body.get("exceptlanguage")?.as_bool()?,
        },
        "SizeSpecification" => Condition::Size {
            min_gib: body.get("min")?.as_f64()?,
            max_gib: body.get("max")?.as_f64()?,
        },
        "YearSpecification" => Condition::Year {
            min: integer("min")?,
            max: integer("max")?,
        },
        "SourceSpecification" => Condition::Source {
            value: integer("value")?,
        },
        "ResolutionSpecification" => Condition::Resolution {
            value: integer("value")?,
        },
        "QualityModifierSpecification" => Condition::QualityModifier {
            value: integer("value")?,
        },
        "IndexerFlagSpecification" => Condition::IndexerFlag {
            value: integer("value")?,
        },
        "ReleaseTypeSpecification" => Condition::ReleaseType {
            value: integer("value")?,
        },
        _ => return None,
    };
    Some(Specification {
        name: body.get("name")?.as_str()?.into(),
        negate: body.get("negate")?.as_bool()?,
        required: body.get("required")?.as_bool()?,
        condition,
    })
}
pub(super) async fn read(
    source: &Source,
    app: Application,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Catalog> {
    let mut result = Catalog {
        definitions: BTreeMap::new(),
        observed: source.tables.contains_key("CustomFormats"),
    };
    let Some(table) = source.tables.get("CustomFormats") else {
        return Ok(result);
    };
    if table.rows.len() > native::MAX_FORMATS {
        return Err(ImportError("snapshot exceeds custom format limit"));
    }
    unsupported.retain(|u| u.table != "CustomFormats");
    let media = if matches!(app, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    for row in &table.rows {
        let id = positive(row, "Id")?;
        if id > 9_007_199_254_740_991 || result.definitions.contains_key(&id) {
            return Err(ImportError(
                "invalid or duplicate source custom format identity",
            ));
        }
        let json = match row.get("Specifications") {
            Some(Value::Text(s)) => Some(
                serde_json::from_str::<J>(s)
                    .map_err(|_| ImportError("malformed source custom format JSON"))?,
            ),
            _ => None,
        };
        let parsed = (|| {
            if row.keys().any(|k| {
                ![
                    "Id",
                    "Name",
                    "Specifications",
                    "IncludeCustomFormatWhenRenaming",
                ]
                .contains(&k.as_str())
            }) {
                return None;
            }
            let specifications = json
                .as_ref()?
                .as_array()?
                .iter()
                .map(specification)
                .collect::<Option<Vec<_>>>()?;
            Some(native::Input {
                name: text(row, "Name").ok()?.into(),
                include_when_renaming: boolean(row, "IncludeCustomFormatWhenRenaming").ok()? == 1,
                specifications,
            })
        })();
        let parsed = if let Some(input) = parsed {
            match native::validate(media, &input).await {
                Ok(json) => Some((input, json)),
                Err(e) if e.0 == axum::http::StatusCode::SERVICE_UNAVAILABLE => {
                    return Err(ImportError(
                        "custom format validation temporarily unavailable; retry snapshot",
                    ));
                }
                Err(_) => None,
            }
        } else {
            None
        };
        if parsed.is_none() {
            super::profiles::issue(
                unsupported,
                "CustomFormats",
                "whole_custom_format_unsupported",
            )
        }
        result.definitions.insert(id, parsed);
    }
    Ok(result)
}
pub(super) async fn write(
    conn: &Connection,
    catalog: &Catalog,
    report: &mut Report,
    activated: bool,
) -> Result<BTreeMap<i64, i64>> {
    let media = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let mut existing = native::catalog(conn, media)
        .await
        .map_err(|_| ImportError("custom format destination read failed"))?;
    let mut ids = BTreeMap::new();
    for (source_id, definition) in &catalog.definitions {
        let Some((input, json)) = definition else {
            continue;
        };
        let mapped=conn.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='custom_formats' AND source_id=?",params![report.application.name(),report.fingerprint.clone(),*source_id]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?;
        let candidate = existing.iter().find(|f| f.definition.name == input.name);
        let id = match candidate {
            Some(f)
                if mapped.is_none_or(|id| id == f.id)
                    && !(activated && mapped.is_none())
                    && f.definition.include_when_renaming == input.include_when_renaming
                    && f.definition.specifications == input.specifications =>
            {
                report.duplicates += 1;
                f.id
            }
            None if mapped.is_none() && !activated => {
                let f = native::persist_validated(conn, media, None, input.clone(), json.clone())
                    .await
                    .map_err(|_| ImportError("custom format destination write failed"))?;
                let id = f.id;
                existing.push(f);
                report.mapped += 1;
                id
            }
            _ => {
                report.conflicts += 1;
                continue;
            }
        };
        conn.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id) VALUES(?,?,'custom_formats',?,?) ON CONFLICT DO NOTHING",params![report.application.name(),report.fingerprint.clone(),*source_id,id]).await?;
        ids.insert(*source_id, id);
    }
    Ok(ids)
}
