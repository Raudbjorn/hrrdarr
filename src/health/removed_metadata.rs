//! Persisted provider removal is diagnostic only; no remote lookup or library mutation.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::db::Database;
use libsql::{TransactionBehavior, params};

const SAMPLE_LIMIT: i64 = 16;
const TITLE_BYTES: usize = 160;
const MESSAGE_BYTES: usize = 4096;

fn display_title(prefix: &str, original_bytes: i64) -> String {
    let mut title = String::new();
    let mut shortened = original_bytes > prefix.len() as i64;
    for character in prefix.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if title.len() + character.len_utf8() > TITLE_BYTES {
            shortened = true;
            break;
        }
        title.push(character);
    }
    let title = title.trim();
    let title = if title.is_empty() {
        "(untitled)"
    } else {
        title
    };
    format!(
        "{title}{}",
        if shortened { " [title truncated]" } else { "" }
    )
}

pub(super) async fn evaluate(
    db: &Database,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>, &'static str> {
    let (source, table, title, source_id, order, noun, compatibility, wiki, reason) =
        match identity.scope {
            HealthScope::Tv => (
                "TVDB",
                "series",
                "title",
                "tvdb_id",
                "id",
                "series",
                "RemovedSeriesCheck",
                "https://wiki.servarr.com/sonarr/system#series-removed-from-thetvdb",
                "removed_series",
            ),
            HealthScope::Movies => (
                "TMDb",
                "movies m JOIN movie_metadata mm ON mm.id=m.metadata_id",
                "mm.title",
                "mm.tmdb_id",
                "m.id",
                "movies",
                "RemovedMovieCheck",
                "https://wiki.servarr.com/radarr/system#movie-was-removed-from-tmdb",
                "removed_movie",
            ),
            _ => return Err("check_failed"),
        };
    if identity.check_key != "removed_metadata" {
        return Err("check_failed");
    }
    let status = if identity.scope == HealthScope::Movies {
        "mm.status"
    } else {
        "status"
    };
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    // Both reads share a snapshot. Only fixed SQL identifiers above are interpolated.
    let total = tx
        .query(
            &format!("SELECT count(*) FROM {table} WHERE {status}='deleted'"),
            (),
        )
        .await
        .map_err(|_| "storage_error")?
        .next()
        .await
        .map_err(|_| "storage_error")?
        .ok_or("check_failed")?
        .get::<i64>(0)
        .map_err(|_| "storage_error")?;
    // Bound the SQL projection before decoding, even for oversized legacy titles.
    let mut rows = tx.query(&format!("SELECT substr({title},1,?),length(CAST({title} AS BLOB)),CASE WHEN typeof({source_id})='integer' THEN {source_id} END,typeof({source_id}) FROM {table} WHERE {status}='deleted' ORDER BY {order} LIMIT ?"), params![TITLE_BYTES as i64 + 1, SAMPLE_LIMIT]).await.map_err(|_| "storage_error")?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        let prefix: String = row.get(0).map_err(|_| "storage_error")?;
        let bytes: i64 = row.get(1).map_err(|_| "storage_error")?;
        let id: Option<i64> = row.get(2).map_err(|_| "storage_error")?;
        let id_type: String = row.get(3).map_err(|_| "storage_error")?;
        if !matches!(id_type.as_str(), "integer" | "null") {
            return Err("check_failed");
        }
        if bytes < 0 || id.is_some_and(|v| v <= 0) {
            return Err("check_failed");
        }
        let id = id.map_or_else(|| "unavailable".into(), |v| v.to_string());
        entries.push(format!(
            "{} ({source} ID {id})",
            display_title(&prefix, bytes)
        ));
    }
    drop(rows);
    tx.commit().await.map_err(|_| "storage_error")?;
    if total == 0 {
        return Ok(None);
    }
    if entries.len() as i64 != total.min(SAMPLE_LIMIT) {
        return Err("check_failed");
    }
    let omitted = total - entries.len() as i64;
    let omission = if omitted > 0 {
        format!("; {omitted} additional titles omitted")
    } else {
        String::new()
    };
    let message = format!(
        "{total} library {noun} marked removed by the metadata provider: {}{omission}. Review the provider identity for duplicate or miscategorized records.",
        entries.join("; ")
    );
    if message.len() > MESSAGE_BYTES {
        return Err("check_failed");
    }
    Ok(Some(HealthIssue {
        identity: identity.clone(),
        severity: HealthSeverity::Error,
        reason: format!(
            "{reason}_{}",
            if total == 1 { "single" } else { "multiple" }
        ),
        message,
        wiki_url: wiki.into(),
        compatibility_type: compatibility.into(),
    }))
}
