//! Activate only whole representable tag catalogs/assignments; preserve raw unsupported facts.
use super::*;
use crate::{
    api::MediaDomain,
    tags::{self as native, Assignment, Mode},
};
use std::collections::BTreeSet;
pub(super) struct Tags {
    catalog: BTreeMap<i64, String>,
    owners: Vec<(i64, Vec<i64>)>,
}
fn issue(u: &mut Vec<Unsupported>, table: &str, reason: &str) {
    profiles::issue(u, table, reason)
}
pub(super) fn read(source: &Source, app: Application, u: &mut Vec<Unsupported>) -> Result<Tags> {
    let owner_table = if matches!(app, Application::Sonarr) {
        "Series"
    } else {
        "Movies"
    };
    let mut catalog = BTreeMap::new();
    let mut supported = true;
    if let Some(table) = source.tables.get("Tags") {
        if table.rows.len() > native::MAX_TAGS {
            return Err(ImportError("source tag catalog exceeds 1024 entries"));
        }
        if table
            .columns
            .iter()
            .any(|c| !["Id", "Label"].contains(&c.as_str()))
        {
            supported = false;
            issue(u, "Tags", "unsupported tag catalog fields");
        }
        let mut labels = BTreeSet::new();
        for row in &table.rows {
            let id = positive(row, "Id")?;
            if id > 9_007_199_254_740_991 {
                return Err(ImportError("unsafe source tag identity"));
            }
            let label = native::label(text(row, "Label")?).map_err(ImportError)?;
            if !labels.insert(label.clone()) || catalog.insert(id, label).is_some() {
                return Err(ImportError(
                    "duplicate source tag identity or normalized label",
                ));
            }
        }
        if supported {
            u.retain(|i| i.table != "Tags");
        }
    }
    let mut owners = vec![];
    if let Some(table) = source.tables.get(owner_table) {
        for row in &table.rows {
            let Some(value) = row.get("Tags") else {
                continue;
            };
            let Value::Text(json) = value else {
                issue(u, owner_table, "Tags: unsupported non-text assignment");
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(json)
                .map_err(|_| ImportError("malformed source tag assignment JSON"))?;
            let Some(values) = value.as_array() else {
                issue(u, owner_table, "Tags: unsupported assignment shape");
                continue;
            };
            if values.len() > native::MAX_ASSIGNMENTS {
                return Err(ImportError("source tag assignment exceeds 200 entries"));
            }
            let ids = values
                .iter()
                .map(|v| {
                    v.as_i64()
                        .filter(|i| *i > 0 && *i <= 9_007_199_254_740_991)
                        .ok_or(ImportError("invalid source tag reference"))
                })
                .collect::<Result<Vec<_>>>()?;
            if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
                return Err(ImportError("duplicate source tag reference"));
            }
            if !source.tables.contains_key("Tags") && !ids.is_empty() {
                issue(
                    u,
                    owner_table,
                    "Tags: missing source catalog; assignment inactive",
                );
                continue;
            }
            if ids.iter().any(|id| !catalog.contains_key(id)) {
                return Err(ImportError("source tag reference is missing from catalog"));
            }
            if supported {
                owners.push((positive(row, "Id")?, ids));
            } else {
                issue(u, owner_table, "Tags: catalog semantics unsupported");
            }
        }
        // Remove only the recognized column from generic reporting; specific unsupported rows remain.
        for item in u.iter_mut().filter(|i| i.table == owner_table) {
            item.columns.retain(|c| c != "Tags");
        }
        u.retain(|i| !i.columns.is_empty());
    }
    if !supported {
        catalog.clear();
    }
    Ok(Tags { catalog, owners })
}
fn native_error(_: crate::qualities::Error) -> ImportError {
    ImportError("tag reconciliation failed")
}
pub(super) async fn write(c: &Connection, plan: &Tags, report: &mut Report) -> Result<()> {
    let app = report.application.name();
    let media = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let (domain, _, owner_key) = native::names(media);
    let owner_table = if matches!(media, MediaDomain::Tv) {
        "series"
    } else {
        "movies"
    };
    let activated = c
        .query(
            "SELECT tag_version FROM snapshot_imports WHERE application=? AND fingerprint=?",
            params![app, report.fingerprint.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("snapshot identity absent"))?
        .get::<i64>(0)?
        == 1;
    let mut mapped = BTreeMap::new();
    for (source_id, label) in &plan.catalog {
        let prior=c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table='tags' AND source_id=?",params![app,report.fingerprint.clone(),*source_id]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?;
        let existing = c
            .query(
                "SELECT id FROM tags WHERE media_type=? AND label=?",
                params![domain, label.clone()],
            )
            .await?
            .next()
            .await?
            .map(|r| r.get::<i64>(0))
            .transpose()?;
        let id = match (prior, existing) {
            (Some(a), Some(b)) if a == b => {
                report.duplicates += 1;
                a
            }
            (None, Some(b)) if !activated => {
                report.duplicates += 1;
                b
            }
            (None, None) if !activated => {
                report.mapped += 1;
                native::insert(c, media, label)
                    .await
                    .map_err(native_error)?
            }
            _ => {
                report.conflicts += 1;
                continue;
            }
        };
        c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id)VALUES(?,?,'tags',?,?) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),*source_id,id]).await?;
        mapped.insert(*source_id, id);
    }
    for (source_owner, ids) in &plan.owners {
        let Some(row)=c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table=? AND source_id=?",params![app,report.fingerprint.clone(),owner_table,*source_owner]).await?.next().await? else{report.conflicts+=1;continue};
        let owner = row.get::<i64>(0)?;
        if c.query(&format!("SELECT 1 FROM {owner_table} WHERE id=?"), [owner])
            .await?
            .next()
            .await?
            .is_none()
        {
            report.conflicts += 1;
            continue;
        }
        let Some(mut wanted) = ids
            .iter()
            .map(|id| mapped.get(id).copied())
            .collect::<Option<Vec<_>>>()
        else {
            report.conflicts += 1;
            continue;
        };
        wanted.sort_unstable();
        let current = native::assigned(c, media, owner)
            .await
            .map_err(native_error)?;
        if current == wanted {
            report.duplicates += 1;
            continue;
        }
        let touched = c
            .query(
                &format!("SELECT 1 FROM library_tag_edits WHERE {owner_key}=?"),
                [owner],
            )
            .await?
            .next()
            .await?
            .is_some();
        if activated || touched || !current.is_empty() {
            report.conflicts += 1;
            continue;
        }
        native::assign(
            c,
            media,
            owner,
            &Assignment {
                mode: Mode::Replace,
                ids: wanted,
            },
            false,
        )
        .await
        .map_err(native_error)?;
        report.metadata_backfilled += 1;
    }
    c.execute(
        "UPDATE snapshot_imports SET tag_version=1 WHERE application=? AND fingerprint=?",
        params![app, report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
