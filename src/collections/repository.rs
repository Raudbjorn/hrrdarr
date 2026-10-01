//! Caller-owned atomic association changes. Never performs network or filesystem actions.
use crate::{library::refresh::Error, metadata::CollectionAssociation};
use libsql::{Connection, params};
use std::path::Path;
type Result<T> = std::result::Result<T, Error>;
const MAX_ID: i64 = 9_007_199_254_740_991;

/// None is ordinary discovery; Some(None) explicitly enables a new collection;
/// Some(Some(revision)) explicitly enables an existing policy under CAS.
pub async fn adopt(
    c: &Connection,
    movie_id: i64,
    metadata_id: i64,
    association: CollectionAssociation,
    enable: Option<Option<i64>>,
) -> Result<bool> {
    if c.is_autocommit()
        || !(1..=MAX_ID).contains(&movie_id)
        || !(1..=MAX_ID).contains(&metadata_id)
    {
        return Err(Error::Conflict);
    }
    let row = c.query("SELECT m.path,s.quality_profile_id,s.minimum_availability FROM movies m LEFT JOIN library_settings s ON s.movie_id=m.id AND s.media_type='movies' WHERE m.id=? AND m.metadata_id=?",params![movie_id,metadata_id]).await?.next().await?.ok_or(Error::TargetChanged)?;
    let path: String = row.get(0)?;
    let profile: Option<i64> = row.get(1)?;
    let availability: Option<String> = row.get(2)?;
    drop(row);
    if matches!(association, CollectionAssociation::Absent) {
        return if enable.is_some() {
            Err(Error::Conflict)
        } else {
            Ok(false)
        };
    }
    let mut rows=c.query("SELECT c.id,c.tmdb_id FROM movie_collection_members m JOIN movie_collections c ON c.id=m.collection_id WHERE m.metadata_id=? LIMIT 1001",[metadata_id]).await?;
    let mut prior = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: i64 = row.get(0)?;
        let remote: Option<i64> = row.get(1)?;
        // Legacy unidentified associations require explicit reconciliation, never title matching.
        if remote.is_none() || prior.len() >= 1000 {
            return Err(Error::Conflict);
        }
        prior.push(id);
    }
    drop(rows);
    let summary = match association {
        CollectionAssociation::Clear => {
            if enable.is_some() {
                return Err(Error::Conflict);
            }
            c.execute(
                "DELETE FROM movie_collection_members WHERE metadata_id=?",
                [metadata_id],
            )
            .await?;
            for id in &prior {
                cleanup_without_library(c, *id).await?;
            }
            return Ok(!prior.is_empty());
        }
        CollectionAssociation::Present(summary) => summary,
        CollectionAssociation::Absent => unreachable!(),
    };
    if !(1..=MAX_ID).contains(&summary.tmdb_id)
        || summary.title.trim().is_empty()
        || summary.title.len() > 512
        || summary.title.chars().any(char::is_control)
    {
        return Err(Error::Conflict);
    }
    let intent = c
        .query(
            "SELECT removed,removal_reason FROM movie_collection_intents WHERE tmdb_id=?",
            [summary.tmdb_id],
        )
        .await?
        .next()
        .await?;
    if let Some(row) = intent {
        let removed = row.get::<i64>(0)? != 0;
        let reason: Option<String> = row.get(1)?;
        drop(row);
        if removed && reason.as_deref() == Some("user") {
            return if enable.is_some() {
                Err(Error::Conflict)
            } else {
                Ok(false)
            };
        }
        if removed {
            // Fresh authoritative discovery may restore a lifecycle removal. Retain
            // local_edit as the independent fence against stale snapshot replay.
            c.execute("UPDATE movie_collection_intents SET removed=0,removal_reason=NULL,revision=revision+1 WHERE tmdb_id=?",[summary.tmdb_id]).await?;
        }
    }
    let existing = c
        .query(
            "SELECT id,title,metadata_revision FROM movie_collections WHERE tmdb_id=?",
            [summary.tmdb_id],
        )
        .await?
        .next()
        .await?;
    let (id, revision, mut changed) = if let Some(row) = existing {
        let id: i64 = row.get(0)?;
        let title: String = row.get(1)?;
        let revision: i64 = row.get(2)?;
        drop(row);
        if title != summary.title {
            if revision == MAX_ID {
                return Err(Error::Conflict);
            }
            c.execute("UPDATE movie_collections SET title=?,metadata_revision=metadata_revision+1 WHERE id=?",params![summary.title,id]).await?;
            (id, revision + 1, true)
        } else {
            (id, revision, false)
        }
    } else {
        c.execute(
            "INSERT INTO movie_collections(tmdb_id,title,added) VALUES(?,?,?)",
            params![
                summary.tmdb_id,
                summary.title,
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
            ],
        )
        .await?;
        (c.last_insert_rowid(), 0, true)
    };
    let policy = c
        .query(
            "SELECT settings_revision FROM movie_collection_settings WHERE collection_id=?",
            [id],
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<i64>(0))
        .transpose()?;
    if policy.is_none() {
        if enable.flatten().is_some() {
            return Err(Error::Conflict);
        }
        let mut roots = c
            .query(
                "SELECT id,path FROM root_folders WHERE media_type='movies' ORDER BY id LIMIT 1001",
                (),
            )
            .await?;
        let mut root = None;
        let mut length = 0;
        let mut count = 0;
        while let Some(row) = roots.next().await? {
            count += 1;
            if count > 1000 {
                return Err(Error::Conflict);
            }
            let candidate: String = row.get(1)?;
            if Path::new(&path).starts_with(Path::new(&candidate)) {
                let n = Path::new(&candidate).components().count();
                if n == length {
                    return Err(Error::Conflict);
                }
                if n > length {
                    root = Some(row.get::<i64>(0)?);
                    length = n;
                }
            }
        }
        drop(roots);
        if enable.is_some() && (root.is_none() || profile.is_none() || availability.is_none()) {
            return Err(Error::Conflict);
        }
        c.execute("INSERT INTO movie_collection_settings(collection_id,root_folder_id,quality_profile_id,minimum_availability,search_on_add,monitored,local_edit) VALUES(?,?,?,?,0,?,?)",params![id,root,profile,availability,i64::from(enable.is_some()),i64::from(enable.is_some())]).await?;
        c.execute("INSERT INTO movie_collection_tags(collection_id,tag_id) SELECT ?,tag_id FROM movie_tags WHERE movie_id=?",params![id,movie_id]).await?;
        changed = true;
    } else if let Some(expected) = enable {
        if expected != policy {
            return Err(Error::Conflict);
        }
        let affected=c.execute("UPDATE movie_collection_settings SET monitored=1,local_edit=1,settings_revision=settings_revision+1 WHERE collection_id=? AND settings_revision=? AND root_folder_id IS NOT NULL AND quality_profile_id IS NOT NULL AND minimum_availability IS NOT NULL AND search_on_add IS NOT NULL",params![id,expected]).await?;
        if affected != 1 {
            return Err(Error::Conflict);
        }
        changed = true;
    }
    c.execute(
        "INSERT INTO movie_collection_intents(tmdb_id) VALUES(?) ON CONFLICT DO NOTHING",
        [summary.tmdb_id],
    )
    .await?;
    if enable.is_some() {
        c.execute("UPDATE movie_collection_intents SET local_edit=1,revision=revision+1 WHERE tmdb_id=? AND local_edit=0",[summary.tmdb_id]).await?;
    }
    for old in prior.iter().filter(|old| **old != id) {
        c.execute(
            "DELETE FROM movie_collection_members WHERE collection_id=? AND metadata_id=?",
            params![old, metadata_id],
        )
        .await?;
        cleanup_without_library(c, *old).await?;
        changed = true;
    }
    if !prior.contains(&id) {
        c.execute("INSERT INTO movie_collection_members(collection_id,metadata_id,origin,metadata_revision) VALUES(?,?,'discovery',?)",params![id,metadata_id,revision]).await?;
        changed = true;
    }
    Ok(changed)
}

/// Call after actual library removal or association change, never file disappearance.
/// Does not delete catalog movie facts, library movies, file associations or history.
pub async fn cleanup_without_library(c: &Connection, id: i64) -> Result<bool> {
    if c.is_autocommit() {
        return Err(Error::Conflict);
    }
    if c.query("SELECT 1 FROM movie_collection_members cm JOIN movies m ON m.metadata_id=cm.metadata_id WHERE cm.collection_id=? LIMIT 1",[id]).await?.next().await?.is_some(){return Ok(false);}
    // Keep native policy authority after live settings disappear.
    let row=c.query("SELECT c.tmdb_id,COALESCE(s.local_edit,0),COALESCE(i.local_edit,0) FROM movie_collections c LEFT JOIN movie_collection_settings s ON s.collection_id=c.id LEFT JOIN movie_collection_intents i ON i.tmdb_id=c.tmdb_id WHERE c.id=?",[id]).await?.next().await?.ok_or(Error::TargetChanged)?;
    let tmdb: Option<i64> = row.get(0)?;
    let local = row.get::<i64>(1)? != 0 || row.get::<i64>(2)? != 0;
    drop(row);
    let tmdb = tmdb.ok_or(Error::Conflict)?;
    c.execute("INSERT INTO movie_collection_intents(tmdb_id,local_edit,removed,removal_reason) VALUES(?,?,1,?) ON CONFLICT(tmdb_id) DO UPDATE SET local_edit=MAX(local_edit,excluded.local_edit),removed=1,removal_reason=CASE WHEN removal_reason='user' THEN 'user' ELSE 'metadata_missing' END,revision=revision+1",params![tmdb,i64::from(local),"metadata_missing"]).await?;
    c.execute(
        "DELETE FROM movie_collection_tags WHERE collection_id=?",
        [id],
    )
    .await?;
    c.execute(
        "DELETE FROM movie_collection_settings WHERE collection_id=?",
        [id],
    )
    .await?;
    c.execute(
        "DELETE FROM movie_collection_members WHERE collection_id=?",
        [id],
    )
    .await?;
    Ok(c.execute("DELETE FROM movie_collections WHERE id=?", [id])
        .await?
        > 0)
}
