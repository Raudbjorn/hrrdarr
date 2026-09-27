//! Whole representable profile reconstruction; raw unsupported policy stays private.
use super::*;
use crate::quality_profiles::{self as native, Cutoff, Item, Leaf, Policy, ProfileInput};
use serde_json::Value as J;

pub(super) struct Profiles {
    table: &'static str,
    profiles: BTreeMap<i64, Option<ProfileInput>>,
    assignments: Vec<(i64, i64)>,
}
fn issue(unsupported: &mut Vec<Unsupported>, table: &str, reason: &str) {
    if let Some(item) = unsupported
        .iter_mut()
        .find(|item| item.table == table && item.columns == [reason])
    {
        item.rows += 1;
    } else {
        unsupported.push(Unsupported {
            table: table.into(),
            rows: 1,
            columns: vec![reason.into()],
        });
    }
}
fn json(row: &Record, key: &str) -> Result<Option<J>> {
    match row.get(key) {
        Some(Value::Text(s)) => serde_json::from_str(s)
            .map(Some)
            .map_err(|_| ImportError("malformed source profile JSON")),
        _ => Ok(None),
    }
}
fn int(row: &Record, key: &str) -> Option<i64> {
    match row.get(key) {
        Some(Value::Integer(n)) => Some(*n),
        _ => None,
    }
}
fn object(value: &J) -> Option<BTreeMap<String, &J>> {
    let mut result = BTreeMap::new();
    for (key, value) in value.as_object()? {
        if result.insert(key.to_ascii_lowercase(), value).is_some() {
            return None;
        }
    }
    Some(result)
}
fn number(obj: &BTreeMap<String, &J>, key: &str) -> Option<Option<f64>> {
    match obj.get(key) {
        None | Some(J::Null) => Some(None),
        Some(v) => v.as_f64().map(Some),
    }
}
fn leaf(value: &J, tv: bool) -> Option<Leaf> {
    let obj = object(value)?;
    if obj.keys().any(|key| {
        ![
            "id",
            "name",
            "quality",
            "items",
            "allowed",
            "minsize",
            "maxsize",
            "preferredsize",
        ]
        .contains(&key.as_str())
    }) {
        return None;
    }
    if obj
        .get("items")
        .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
        || obj.get("id").is_some_and(|v| v.as_i64() != Some(0))
        || obj
            .get("name")
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    {
        return None;
    }
    if !tv
        && ["minsize", "maxsize", "preferredsize"]
            .iter()
            .any(|key| obj.contains_key(*key))
    {
        return None;
    }
    Some(Leaf {
        quality_id: obj.get("quality")?.as_i64()?,
        allowed: obj.get("allowed")?.as_bool()?,
        min_size: number(&obj, "minsize")?,
        max_size: number(&obj, "maxsize")?,
        preferred_size: number(&obj, "preferredsize")?,
    })
}
fn parse(row: &Record, items: &J, formats: &J, tv: bool, old: bool) -> Option<ProfileInput> {
    if !formats.as_array()?.is_empty() {
        return None;
    }
    let roots_json = items.as_array()?;
    if roots_json.len() > native::MAX_NODES {
        return None;
    }
    let count = roots_json.len().checked_add(
        roots_json
            .iter()
            .filter_map(|v| object(v))
            .filter_map(|obj| obj.get("items").and_then(|v| v.as_array()).map(Vec::len))
            .sum::<usize>(),
    )?;
    if count > native::MAX_NODES {
        return None;
    }
    let cutoff = i64::from(i32::try_from(int(row, "Cutoff")?).ok()?);
    let mut roots = Vec::new();
    let mut selected = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    for (position, value) in items.as_array()?.iter().enumerate() {
        let obj = object(value)?;
        if obj.get("quality").is_some_and(|v| !v.is_null()) {
            let value = leaf(value, tv)?;
            if !identities.insert(value.quality_id) {
                return None;
            }
            if cutoff == value.quality_id {
                selected.push(Cutoff::Quality {
                    quality_id: value.quality_id,
                });
            }
            roots.push(Item::Quality(value));
        } else {
            if obj
                .keys()
                .any(|key| !["id", "name", "quality", "items", "allowed"].contains(&key.as_str()))
            {
                return None;
            }
            let id = obj.get("id")?.as_i64()?;
            if !(1..=i64::from(i32::MAX)).contains(&id) || !identities.insert(id) {
                return None;
            }
            let children = obj
                .get("items")?
                .as_array()?
                .iter()
                .map(|v| leaf(v, tv))
                .collect::<Option<Vec<_>>>()?;
            if cutoff == id {
                selected.push(Cutoff::Group { position });
            }
            roots.push(Item::Group {
                name: obj.get("name")?.as_str()?.into(),
                allowed: obj.get("allowed")?.as_bool()?,
                items: children,
            });
        }
    }
    if selected.len() != 1 {
        return None;
    }
    Some(ProfileInput {
        name: match row.get("Name")? {
            Value::Text(s) => s.clone(),
            _ => return None,
        },
        items: roots,
        policy: Some(Policy {
            upgrade_allowed: match int(row, "UpgradeAllowed")? {
                0 => false,
                1 => true,
                _ => return None,
            },
            cutoff: selected.pop()?,
            min_format_score: i32::try_from(int(row, "MinFormatScore")?).ok()?,
            cutoff_format_score: i32::try_from(int(row, "CutoffFormatScore")?).ok()?,
            min_upgrade_format_score: if old && !row.contains_key("MinUpgradeFormatScore") {
                1
            } else {
                i32::try_from(int(row, "MinUpgradeFormatScore")?).ok()?
            },
            language_id: if tv {
                None
            } else {
                Some(i32::try_from(int(row, "Language")?).ok()?)
            },
            format_items: vec![],
        }),
    })
}
pub(super) fn read(
    source: &Source,
    app: Application,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Profiles> {
    let tv = matches!(app, Application::Sonarr);
    let old = !tv && source.version == 206;
    let table = if old { "Profiles" } else { "QualityProfiles" };
    let mut result = Profiles {
        table,
        profiles: BTreeMap::new(),
        assignments: Vec::new(),
    };
    let catalog_empty = source
        .tables
        .get("CustomFormats")
        .is_some_and(|t| t.rows.is_empty());
    if let Some(data) = source.tables.get(table) {
        if data.rows.len() > 256 {
            return Err(ImportError("snapshot exceeds 256 profile limit"));
        }
        unsupported.retain(|item| item.table != table);
        for row in &data.rows {
            let id = int(row, "Id")
                .filter(|id| (1..=9007199254740991).contains(id))
                .ok_or(ImportError("invalid source profile identity"))?;
            if result.profiles.contains_key(&id) {
                return Err(ImportError("duplicate source profile identity"));
            }
            // Malformed serialized fields are errors even when their semantics are unsupported.
            let items = json(row, "Items")?;
            let formats = json(row, "FormatItems")?;
            let known = [
                "Id",
                "Name",
                "Items",
                "UpgradeAllowed",
                "Cutoff",
                "MinFormatScore",
                "CutoffFormatScore",
                "MinUpgradeFormatScore",
                "FormatItems",
            ];
            let unknown = row
                .keys()
                .any(|key| !known.contains(&key.as_str()) && !(key == "Language" && !tv));
            let input = if catalog_empty && !unknown {
                items
                    .as_ref()
                    .zip(formats.as_ref())
                    .and_then(|(i, f)| parse(row, i, f, tv, old))
            } else {
                None
            };
            let input = input.filter(|input| {
                native::validate_input(input, if tv { "tv" } else { "movies" }).is_ok()
            });
            if input.is_none() {
                issue(
                    unsupported,
                    table,
                    if !catalog_empty {
                        "custom_format_catalog_unresolved"
                    } else {
                        "whole_profile_unsupported"
                    },
                );
            }
            result.profiles.insert(id, input);
        }
    }
    let library = if tv { "Series" } else { "Movies" };
    let column = if old { "ProfileId" } else { "QualityProfileId" };
    for entry in unsupported
        .iter_mut()
        .filter(|entry| entry.table == library)
    {
        entry.columns.retain(|name| name != column);
    }
    unsupported.retain(|entry| entry.table != library || !entry.columns.is_empty());
    if let Some(data) = source.tables.get(library) {
        for row in &data.rows {
            if let Some(profile) = int(row, column) {
                let owner =
                    int(row, "Id").ok_or(ImportError("invalid source profile assignment"))?;
                if profile > 0 && result.profiles.get(&profile).is_some_and(Option::is_some) {
                    result.assignments.push((owner, profile));
                } else {
                    issue(unsupported, library, column);
                }
            } else if row.contains_key(column) && !matches!(row.get(column), Some(Value::Null)) {
                issue(unsupported, library, column);
            }
        }
    }
    Ok(result)
}

pub(super) struct Prepared {
    old: bool,
    assignments: Vec<(i64, i64)>,
}
pub(super) async fn prepare(
    conn: &Connection,
    profiles: &Profiles,
    entities: &mut [Entity],
    report: &mut Report,
) -> Result<Prepared> {
    let app = report.application.name();
    let media = if matches!(report.application, Application::Sonarr) {
        "tv"
    } else {
        "movies"
    };
    let prior = conn
        .query(
            "SELECT profile_version FROM snapshot_imports WHERE application=? AND fingerprint=?",
            params![app, report.fingerprint.clone()],
        )
        .await?
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?;
    let old = prior == Some(0);
    conn.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version,episode_metadata_version) VALUES(?,?,?,1) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),report.schema_version]).await?;
    let mut available = std::collections::BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT quality_id FROM quality_definitions WHERE media_type=?",
            [media],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        available.insert(row.get::<i64>(0)?);
    }
    drop(rows);
    let mut ids = BTreeMap::new();
    for (source_id, input) in &profiles.profiles {
        let Some(input) = input else { continue };
        // Catalog checks use the same engine/domain catalogue as native writes.
        let mut valid = true;
        for item in &input.items {
            let leaves = match item {
                Item::Quality(leaf) => std::slice::from_ref(leaf),
                Item::Group { items, .. } => items.as_slice(),
            };
            for leaf in leaves {
                if !available.contains(&leaf.quality_id) {
                    valid = false;
                }
            }
        }
        if !valid {
            issue(
                &mut report.unsupported,
                profiles.table,
                "whole_profile_unsupported",
            );
            continue;
        }
        let mapped=conn.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='quality_profiles' AND source_id=?",params![app,report.fingerprint.clone(),*source_id]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?;
        let candidate = conn
            .query(
                "SELECT id FROM quality_profiles WHERE media_type=? AND name=?",
                params![media, input.name.clone()],
            )
            .await?
            .next()
            .await?
            .map(|r| r.get::<i64>(0))
            .transpose()?;
        let id = match candidate {
            Some(id)
                if mapped.is_none_or(|mapped| mapped == id)
                    && !(prior == Some(1) && mapped.is_none()) =>
            {
                let actual = native::as_input(
                    native::fetch(conn, media, id)
                        .await
                        .map_err(|_| ImportError("profile destination read failed"))?,
                );
                if actual != *input {
                    report.conflicts += 1;
                    continue;
                }
                report.duplicates += 1;
                id
            }
            None if mapped.is_none() && prior != Some(1) => {
                let profile = native::persist_on_connection(conn, media, None, input.clone())
                    .await
                    .map_err(|_| ImportError("profile destination write failed"))?;
                report.mapped += 1;
                profile.id
            }
            _ => {
                report.conflicts += 1;
                continue;
            }
        };
        conn.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id) VALUES(?,?,'quality_profiles',?,?) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),*source_id,id]).await?;
        ids.insert(*source_id, id);
    }
    let settings_indices: BTreeMap<i64, usize> = entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| entity.table == "library_settings")
        .map(|(index, entity)| (entity.source_id, index))
        .collect();
    let mut assignments = Vec::new();
    for (owner, source_profile) in &profiles.assignments {
        if let Some(id) = ids.get(source_profile) {
            if prior.is_some() && conn.query("SELECT 1 FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='library_settings' AND source_id=?",params![app,report.fingerprint.clone(),*owner]).await?.next().await?.is_none() {report.conflicts+=1;continue;}
            if old && conn.query("SELECT 1 FROM snapshot_mappings m JOIN library_settings s ON s.id=m.destination_id WHERE m.application=? AND m.fingerprint=? AND m.destination_table='library_settings' AND m.source_id=? AND s.quality_profile_id IS NULL",params![app,report.fingerprint.clone(),*owner]).await?.next().await?.is_none() {report.conflicts+=1;continue;}
            assignments.push((*owner, *id));
            if !old {
                if let Some(index) = settings_indices.get(owner) {
                    if let Some((_, field)) = entities[*index]
                        .fields
                        .iter_mut()
                        .find(|(name, _)| *name == "quality_profile_id")
                    {
                        *field = Field::Value((*id).into());
                    }
                }
            }
        } else if profiles
            .profiles
            .get(source_profile)
            .is_some_and(Option::is_some)
        {
            issue(
                &mut report.unsupported,
                if media == "tv" { "Series" } else { "Movies" },
                if media == "movies" && report.schema_version == 206 {
                    "ProfileId"
                } else {
                    "QualityProfileId"
                },
            );
        }
    }
    Ok(Prepared { old, assignments })
}
pub(super) async fn finish(
    conn: &Connection,
    prepared: Prepared,
    report: &mut Report,
) -> Result<()> {
    if report.conflicts > 0 {
        return Ok(());
    }
    if prepared.old {
        for (owner, profile) in prepared.assignments {
            let row=conn.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='library_settings' AND source_id=?",params![report.application.name(),report.fingerprint.clone(),owner]).await?.next().await?.ok_or(ImportError("profile assignment mapping absent"))?;
            let id = row.get::<i64>(0)?;
            drop(row);
            if conn.execute("UPDATE library_settings SET quality_profile_id=? WHERE id=? AND quality_profile_id IS NULL",params![profile,id]).await?!=1 {return Err(ImportError("profile backfill assignment changed"));}
            report.metadata_backfilled += 1;
        }
    }
    conn.execute(
        "UPDATE snapshot_imports SET profile_version=1 WHERE application=? AND fingerprint=?",
        params![report.application.name(), report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
