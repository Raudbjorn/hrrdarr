//! A source configuration singleton never overwrites local or earlier imported intent.
use super::*;
use crate::{
    api::MediaDomain,
    revision_policy::{self as native, Mode},
};
pub(super) fn read(source: &Source, u: &mut Vec<Unsupported>) -> Result<Option<Mode>> {
    let Some(table) = source.tables.get("Config") else {
        profiles::issue(u, "Config", "revision policy settings unavailable");
        return Ok(None);
    };
    if !["Key", "Value"]
        .iter()
        .all(|k| table.columns.iter().any(|c| c == k))
    {
        return Err(ImportError("invalid source Config shape"));
    }
    let mut found = None;
    for row in &table.rows {
        if !text(row, "Key")?.eq_ignore_ascii_case("downloadpropersandrepacks") {
            continue;
        }
        if found.is_some() {
            return Err(ImportError("duplicate revision policy key"));
        }
        let value = text(row, "Value")?;
        let mode = match value.to_ascii_lowercase().as_str() {
            "preferandupgrade" | "0" => Some(Mode::PreferAndUpgrade),
            "donotupgrade" | "1" => Some(Mode::DoNotUpgrade),
            "donotprefer" | "2" => Some(Mode::DoNotPrefer),
            _ => None,
        };
        found = Some(mode);
    }
    if table
        .columns
        .iter()
        .any(|c| !["Id", "Key", "Value"].contains(&c.as_str()))
    {
        profiles::issue(u, "Config", "revision policy has unsupported source fields");
        return Ok(None);
    }
    match found {
        Some(None) => {
            profiles::issue(u, "Config", "unsupported revision policy mode");
            Ok(None)
        }
        Some(Some(m)) => Ok(Some(m)),
        None => Ok(Some(Mode::PreferAndUpgrade)),
    }
}
pub(super) async fn write(c: &Connection, mode: Option<Mode>, report: &mut Report) -> Result<()> {
    let Some(mode) = mode else { return Ok(()) };
    let app = report.application.name();
    let media = if matches!(report.application, Application::Sonarr) {
        MediaDomain::Tv
    } else {
        MediaDomain::Movies
    };
    let current = native::read(c, media)
        .await
        .map_err(|_| ImportError("revision policy destination read failed"))?;
    if current.mode == mode {
        report.duplicates += 1;
    } else {
        let touched = c
            .query(
                "SELECT locally_edited FROM revision_policies WHERE media_type=?",
                [native::domain(media)],
            )
            .await?
            .next()
            .await?
            .ok_or(ImportError("revision policy missing"))?
            .get::<i64>(0)?
            != 0;
        let activated=c.query("SELECT 1 FROM snapshot_imports WHERE application=? AND revision_policy_version=1 LIMIT 1",[app]).await?.next().await?.is_some();
        if touched || activated {
            report.conflicts += 1;
            return Ok(());
        }
        native::set(c, media, mode, current.revision, false)
            .await
            .map_err(|_| ImportError("revision policy destination update failed"))?;
        report.metadata_backfilled += 1;
    }
    c.execute("UPDATE snapshot_imports SET revision_policy_version=1 WHERE application=? AND fingerprint=?",params![app,report.fingerprint.clone()]).await?;
    Ok(())
}
