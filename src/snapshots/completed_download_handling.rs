//! Preserve source effective boolean AND definedness; never activate unknown source intent.
use super::*;
use crate::{api::MediaDomain, completed_download_handling as native};
#[derive(Clone, Copy)]
pub(super) struct Plan {
    enabled: bool,
    defined: bool,
}
pub(super) fn read(source: &Source, issues: &mut Vec<Unsupported>) -> Result<Option<Plan>> {
    let Some(table) = source.tables.get("Config") else {
        profiles::issue(
            issues,
            "Config",
            "completed download handling settings unavailable",
        );
        return Ok(None);
    };
    if !["Key", "Value"]
        .iter()
        .all(|k| table.columns.iter().any(|c| c == k))
    {
        return Err(ImportError("invalid source Config shape"));
    }
    let mut found = None;
    let mut unsupported = table
        .columns
        .iter()
        .any(|c| !["Id", "Key", "Value"].contains(&c.as_str()));
    for row in &table.rows {
        let key = text(row, "Key")?;
        if !key.eq_ignore_ascii_case("enablecompleteddownloadhandling") {
            continue;
        }
        if found.is_some() {
            return Err(ImportError("duplicate completed download handling key"));
        }
        // Upstream lookup lowercases cached values but IsDefined queries exact lowercase keys.
        if key != "enablecompleteddownloadhandling" {
            unsupported = true;
        }
        let enabled = match row.get("Value") {
            Some(Value::Null) => true,
            Some(Value::Text(v)) if v.is_empty() => true,
            Some(Value::Text(v)) => {
                if !v.is_ascii() || v.contains('\0') {
                    unsupported = true;
                    true
                } else if v.trim().eq_ignore_ascii_case("true") {
                    true
                } else if v.trim().eq_ignore_ascii_case("false") {
                    false
                } else {
                    return Err(ImportError("invalid completed download handling boolean"));
                }
            }
            _ => return Err(ImportError("invalid completed download handling value")),
        };
        found = Some(enabled);
    }
    if unsupported {
        profiles::issue(
            issues,
            "Config",
            "completed download handling has unsupported source encoding or fields",
        );
        return Ok(None);
    }
    Ok(Some(Plan {
        enabled: found.unwrap_or(true),
        defined: found.is_some(),
    }))
}
pub(super) async fn write(c: &Connection, plan: Option<Plan>, report: &mut Report) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    let app = report.application.name();
    let m = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let current = native::read(c, m)
        .await
        .map_err(|_| ImportError("completed download handling destination read failed"))?;
    let exact = c
        .query(
            "SELECT cdh_version FROM snapshot_imports WHERE application=? AND fingerprint=?",
            params![app, report.fingerprint.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("snapshot provenance missing"))?
        .get::<i64>(0)?
        == 1;
    let prior = c
        .query(
            "SELECT 1 FROM snapshot_imports WHERE application=? AND cdh_version=1 LIMIT 1",
            [app],
        )
        .await?
        .next()
        .await?
        .is_some();
    if exact {
        if current.enabled == plan.enabled && current.defined == plan.defined {
            report.duplicates += 1
        } else {
            report.conflicts += 1
        }
        return Ok(());
    }
    // Equal native saves also fence source activation; equality must not manufacture provenance.
    if current.locally_edited || prior {
        if current.enabled == plan.enabled && current.defined == plan.defined {
            report.duplicates += 1;
        } else {
            report.conflicts += 1;
        }
        // Equality permits unrelated import work, but never adds source authority over local intent.
        return Ok(());
    }
    if current.enabled == plan.enabled && current.defined == plan.defined {
        report.duplicates += 1;
        native::reconcile(c, None, Some(m))
            .await
            .map_err(|_| ImportError("completed download handling reconciliation failed"))?;
    } else {
        native::set(c, m, plan.enabled, plan.defined, current.revision, false)
            .await
            .map_err(|_| ImportError("completed download handling activation failed"))?;
        report.metadata_backfilled += 1;
    }
    c.execute(
        "UPDATE snapshot_imports SET cdh_version=1 WHERE application=? AND fingerprint=?",
        params![app, report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
