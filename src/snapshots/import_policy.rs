//! Config is archived unchanged; only a complete supported policy gains authority.
use super::*;
use crate::{api::MediaDomain, import_policy as native};

pub(super) struct Plan {
    skip: bool,
    minimum: i64,
    folders: String,
}

pub(super) fn read(source: &Source, issues: &mut Vec<Unsupported>) -> Result<Option<Plan>> {
    let Some(table) = source.tables.get("Config") else {
        profiles::issue(issues, "Config", "import policy settings unavailable");
        return Ok(None);
    };
    if !["Key", "Value"]
        .iter()
        .all(|key| table.columns.iter().any(|c| c == key))
    {
        return Err(ImportError("invalid source Config shape"));
    }
    let mut plan = Plan {
        skip: false,
        minimum: native::MINIMUM_FREE_SPACE_MIB,
        folders: native::DEFAULT_WORKING_FOLDERS.into(),
    };
    let mut seen = [false; 3];
    let mut unsupported = table
        .columns
        .iter()
        .any(|c| !["Id", "Key", "Value"].contains(&c.as_str()));
    for row in &table.rows {
        let key = text(row, "Key")?;
        let index = match key.to_ascii_lowercase().as_str() {
            "skipfreespacecheckwhenimporting" => 0,
            "minimumfreespacewhenimporting" => 1,
            "downloadclientworkingfolders" => 2,
            _ => continue,
        };
        if seen[index] {
            return Err(ImportError("duplicate import policy key"));
        }
        seen[index] = true;
        // Source Config defaults only the whole empty value; do not trim prefixes.
        let value = match row.get("Value") {
            Some(Value::Null) => continue,
            Some(Value::Text(value)) if value.is_empty() => continue,
            Some(Value::Text(value)) => value,
            _ => {
                unsupported = true;
                continue;
            }
        };
        match index {
            0 if value.trim().eq_ignore_ascii_case("true") => plan.skip = true,
            0 if value.trim().eq_ignore_ascii_case("false") => plan.skip = false,
            1 => match value.trim().parse::<i32>() {
                Ok(n) if i64::from(n) >= native::MINIMUM_FREE_SPACE_MIB => {
                    plan.minimum = i64::from(n)
                }
                _ => unsupported = true,
            },
            2 => match native::canonical_working_folders(value) {
                Ok(folders) => plan.folders = folders.into(),
                Err(_) => unsupported = true,
            },
            _ => unsupported = true,
        }
    }
    if unsupported {
        profiles::issue(
            issues,
            "Config",
            "import policy has unsupported source values or fields",
        );
        return Ok(None);
    }
    Ok(Some(plan))
}

pub(super) async fn write(c: &Connection, plan: Option<&Plan>, report: &mut Report) -> Result<()> {
    let Some(plan) = plan else { return Ok(()) };
    let app = report.application.name();
    let domain = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let current = native::read(c, domain)
        .await
        .map_err(|_| ImportError("import policy destination read failed"))?;
    let equal = current.skip_free_space_check_when_importing == plan.skip
        && current.minimum_free_space_mib == plan.minimum
        && current.download_client_working_folders == plan.folders;
    let exact = c.query("SELECT import_policy_version FROM snapshot_imports WHERE application=? AND fingerprint=?", params![app,report.fingerprint.clone()]).await?.next().await?.ok_or(ImportError("snapshot provenance missing"))?.get::<i64>(0)? == 1;
    let prior = c.query("SELECT 1 FROM snapshot_imports WHERE application=? AND import_policy_version=1 LIMIT 1",[app]).await?.next().await?.is_some();
    let local = c
        .query(
            "SELECT locally_edited FROM import_policies WHERE media_type=?",
            [native::domain(domain)],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("import policy missing"))?
        .get::<i64>(0)?
        != 0;
    if exact || prior || local {
        if equal {
            report.duplicates += 1;
        } else {
            report.conflicts += 1;
        }
        // Equal local saves do not manufacture source provenance either.
        return Ok(());
    }
    if equal {
        report.duplicates += 1;
    } else {
        let update = native::Update {
            revision: current.revision,
            skip_free_space_check_when_importing: plan.skip,
            minimum_free_space_mib: plan.minimum,
            download_client_working_folders: plan.folders.clone(),
        };
        native::set(c, domain, &update, false)
            .await
            .map_err(|_| ImportError("import policy activation failed"))?;
        report.metadata_backfilled += 1;
    }
    c.execute(
        "UPDATE snapshot_imports SET import_policy_version=1 WHERE application=? AND fingerprint=?",
        params![app, report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
