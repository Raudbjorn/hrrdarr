//! Migration 0080 (import lists, array position 79): upgrade from the previous schema keeps rows,
//! the migration is atomic, and the table constraints and triggers hold.
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
const LIST: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";

async fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
fn list_sql(id: &str, media: &str, name: &str, with_defaults: bool) -> String {
    let (columns, values) = if media == "tv" {
        ("series_type,season_folder", "'standard',1")
    } else {
        ("minimum_availability", "'released'")
    };
    let (extra_columns, extra_values) = if with_defaults {
        (",quality_profile_id,root_folder_id", ",1,1")
    } else {
        ("", "")
    };
    format!(
        "INSERT INTO import_lists(id,media_type,name,implementation,settings_json,created_at,updated_at,{columns}{extra_columns}) VALUES('{id}','{media}','{name}','custom','{{}}',1,1,{values}{extra_values})"
    )
}

#[tokio::test]
async fn import_lists_upgrade_from_previous_schema_is_atomic_and_preserves_rows()
-> Result<(), Error> {
    let s = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-lists-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&s.0)?;
    let path = s.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    // Mirror the runner: one transaction with foreign keys suspended, history rows alongside.
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    let tx = c.transaction().await?;
    tx.execute(HISTORY_SQL, ()).await?;
    // Keep testing the original import-lists boundary after newer migrations append.
    let latest = MIGRATIONS
        .iter()
        .position(|(name, _)| *name == "import_lists")
        .unwrap();
    assert_eq!(MIGRATIONS[latest].0, "import_lists");
    for (i, (name, sql)) in MIGRATIONS.iter().take(latest).enumerate() {
        tx.execute_batch(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute_batch("INSERT INTO series(id,title,path,tvdb_id)VALUES(1,'Kept','/kept',9);INSERT INTO tags(id,media_type,label)VALUES(5,'tv','kept-tag'),(6,'movies','kept-tag');").await?;
    // The migration applies atomically: applied twice inside one transaction it fails, and the
    // transaction rolls back to the previous schema.
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[latest].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[latest].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'import_list%'"
        )
        .await,
        0
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 83); // Both published tails append without rewriting existing ledger entries.
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE tvdb_id=9").await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_lists").await, 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM tags").await, 2);
    drop(c);
    drop(db);
    // Reopening is a no-op.
    let db = Database::open_local(&path).await?;
    assert_eq!(version(&db.connect().await?).await?, 83); // Both published tails append without rewriting existing ledger entries.
    Ok(())
}

#[tokio::test]
async fn search_outbox_upgrades_both_published_histories_atomically() -> Result<(), Error> {
    use super::published_prefix_tests::{Scratch, seed_lineage};

    async fn witness(c: &Connection, prefix: usize) -> Result<Vec<Vec<libsql::Value>>, Error> {
        let mut values = Vec::new();
        for sql in [
            format!("SELECT * FROM schema_migrations WHERE version<={prefix} ORDER BY version"),
            "SELECT * FROM import_list_additions ORDER BY id".into(),
        ] {
            let mut rows = c.query(&sql, ()).await?;
            while let Some(row) = rows.next().await? {
                values.push(
                    (0..row.column_count())
                        .map(|i| row.get_value(i))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
        }
        Ok(values)
    }

    // Both deployed tips are immutable; the same outbox migration appends at different ordinals.
    for (history, prefix) in [
        (MIGRATIONS, 79),
        (MAIN_MIGRATIONS, 82),
        (LEGACY_MIGRATIONS, 83),
    ] {
        let files = Scratch::new();
        let path = files.0.join("db");
        let (raw, c) = seed_lineage(&path, history, prefix).await?;
        c.execute_batch(
            "INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv'),(2,102,'No search','/tv2');
             INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,101,'Movie');
             INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');
             INSERT INTO import_list_additions(id,media_type,series_id,movie_id,list_name,external_id,search_requested,added_at) VALUES
               (1,'tv',1,NULL,'Deleted TV list',101,1,100),
               (2,'movies',NULL,1,'Deleted movie list',101,1,200),
               (3,'tv',2,NULL,'No search list',102,0,300);",
        )
        .await?;
        let before = witness(&c, prefix).await?;
        c.execute(&format!("CREATE TRIGGER fail_outbox BEFORE INSERT ON schema_migrations WHEN NEW.version={} BEGIN SELECT RAISE(ABORT,'outbox fixture rollback'); END", prefix+1), ()).await?;
        drop(c);
        drop(raw);
        assert!(
            Database::open_local(&path)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("rolled back")
        );
        let raw = libsql::Builder::new_local(&path).build().await?;
        let c = raw.connect()?;
        assert_eq!(version(&c).await?, prefix);
        assert_eq!(witness(&c, prefix).await?, before);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'import_list_search_%'"
            )
            .await,
            0
        );
        c.execute("DROP TRIGGER fail_outbox", ()).await?;
        drop(c);
        drop(raw);

        for reopen in 0..2 {
            let db = Database::open_local(&path).await?;
            let c = db.connect().await?;
            assert_eq!(version(&c).await?, history.len()); // Upgrade also applies the other published tail.
            assert_eq!(db.migration_backup().is_some(), reopen == 0);
            assert_eq!(
                witness(&c, prefix).await?,
                before,
                "ledger and recorded user intent are unchanged"
            );
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM import_list_search_requests").await,
                2
            );
            assert_eq!(scalar(&c, "SELECT count(*) FROM import_list_search_requests WHERE status='pending' AND next_attempt_at=0 AND error_code IS NULL AND admitted_at IS NULL AND ((addition_id=1 AND created_at=100) OR (addition_id=2 AND created_at=200))").await, 2);
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM import_list_search_commands").await,
                0,
                "migration records pending intent without admitting work"
            );
            integrity(&c).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn import_list_constraints_triggers_and_cascades() -> Result<(), Error> {
    let s =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-lists-rules-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&s.0)?;
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO tags(id,media_type,label)VALUES(5,'tv','a'),(6,'movies','a');INSERT INTO quality_profiles(id,media_type,name)VALUES(1,'tv','P'),(2,'movies','P');INSERT INTO root_folders(id,media_type,path)VALUES(1,'tv','/tv'),(2,'movies','/movies');").await?;
    c.execute(&list_sql(LIST, "tv", "A", true), ()).await?;
    // Domain-specific columns are pinned to their own domain.
    for sql in [
        "UPDATE import_lists SET minimum_availability='released'",
        "UPDATE import_lists SET monitor='bogus'",
        "UPDATE import_lists SET interval_minutes=1",
        "UPDATE import_lists SET enabled=2",
        "UPDATE import_lists SET media_type='movies'",
        "UPDATE import_lists SET implementation='rss'",
        "UPDATE import_lists SET revision=revision+2",
        "UPDATE import_lists SET last_error='Has Spaces'",
        "UPDATE import_lists SET settings_json='[]'",
        "UPDATE import_lists SET credentials=x'00'",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    assert!(c.execute("INSERT INTO import_lists(id,media_type,name,implementation,settings_json,created_at,updated_at) VALUES('22222222-2222-4222-8222-222222222222','movies','M','custom','{}',1,1)", ()).await.is_err(), "a movie list needs a minimum availability");
    assert!(c.execute("INSERT INTO import_lists(id,media_type,name,implementation,settings_json,created_at,updated_at,series_type,season_folder) VALUES('22222222-2222-4222-8222-222222222222','tv','M','custom','{}',1,1,NULL,NULL)", ()).await.is_err(), "a tv list needs series type and season folder");
    c.execute("INSERT INTO import_lists(id,media_type,name,implementation,settings_json,created_at,updated_at,minimum_availability,monitor) VALUES(?,'movies','M','custom','{}',1,1,'released','none')", [OTHER]).await?;
    assert!(
        c.execute(
            "UPDATE import_lists SET monitor='first_season' WHERE media_type='movies'",
            ()
        )
        .await
        .is_err()
    );
    // Names are unique per domain, case-insensitively, not across domains.
    assert!(
        c.execute(
            &list_sql("33333333-3333-4333-8333-333333333333", "tv", "a", false),
            ()
        )
        .await
        .is_err()
    );
    // Sync bookkeeping does not advance the revision; configuration does, by exactly one.
    c.execute(
        "UPDATE import_lists SET last_sync_at=5,last_error='timeout' WHERE id=?",
        [LIST],
    )
    .await?;
    c.execute("UPDATE import_lists SET revision=2 WHERE id=?", [LIST])
        .await?;
    // Tags are domain-qualified on both sides.
    c.execute(
        "INSERT INTO import_list_tags(list_id,media_type,tag_id) VALUES(?,'tv',5)",
        [LIST],
    )
    .await?;
    assert!(
        c.execute(
            "INSERT INTO import_list_tags(list_id,media_type,tag_id) VALUES(?,'tv',6)",
            [LIST]
        )
        .await
        .is_err(),
        "movie tag on a tv list"
    );
    assert!(
        c.execute(
            "INSERT INTO import_list_tags(list_id,media_type,tag_id) VALUES(?,'movies',6)",
            [LIST]
        )
        .await
        .is_err(),
        "list/domain mismatch"
    );
    c.execute("DELETE FROM tags WHERE id=5", ()).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM import_list_tags").await,
        0,
        "a deleted tag leaves the default set"
    );
    // Profile and root deletion disable adding.
    c.execute("DELETE FROM quality_profiles WHERE id=1", ())
        .await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_lists WHERE id='11111111-1111-4111-8111-111111111111' AND quality_profile_id IS NULL AND enable_automatic_add=0").await, 1);
    // Exclusions: unique per (domain, id), the same number in the other domain is separate.
    c.execute("INSERT INTO import_list_exclusions(media_type,external_id,title,created_at) VALUES('tv',10,'T',1),('movies',10,'M',1)", ()).await?;
    assert!(c.execute("INSERT INTO import_list_exclusions(media_type,external_id,title,created_at) VALUES('tv',10,'T2',1)", ()).await.is_err());
    assert!(c.execute("INSERT INTO import_list_exclusions(media_type,external_id,title,created_at) VALUES('tv',0,'T',1)", ()).await.is_err());
    // One active run per list; terminal rows are pruned to 32 per list.
    let run = |id: &str, status: &str| {
        format!(
            "INSERT INTO import_list_runs(id,list_id,media_type,origin,status,next_attempt_at,created_at,completed_at) VALUES('{id}','{LIST}','tv','manual','{status}',0,{},{})",
            id.len(),
            if matches!(status, "succeeded" | "failed") {
                "1"
            } else {
                "NULL"
            }
        )
    };
    c.execute(&run("a0000000-0000-4000-8000-000000000000", "queued"), ())
        .await?;
    assert!(
        c.execute(&run("a0000000-0000-4000-8000-000000000001", "running"), ())
            .await
            .is_err()
    );
    assert!(
        c.execute(
            &run("a0000000-0000-4000-8000-000000000002", "succeeded"),
            ()
        )
        .await
        .is_ok()
    );
    assert!(
        c.execute(
            "UPDATE import_list_runs SET status='succeeded' WHERE status='queued'",
            ()
        )
        .await
        .is_err(),
        "terminal rows need completed_at"
    );
    for i in 0..40 {
        c.execute(&format!("INSERT INTO import_list_runs(id,list_id,media_type,origin,status,next_attempt_at,created_at,completed_at) VALUES('b{i:07}-0000-4000-8000-000000000000','{LIST}','tv','scheduled','queued',0,{i},NULL)"), ()).await.ok();
        c.execute(
            "UPDATE import_list_runs SET status='failed',completed_at=2 WHERE status='queued'",
            (),
        )
        .await?;
    }
    assert!(
        scalar(
            &c,
            "SELECT count(*) FROM import_list_runs WHERE status IN ('succeeded','failed')"
        )
        .await
            <= 32
    );
    // Item snapshot capacity.
    c.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<5000) INSERT INTO import_list_items(list_id,key,seq,title,updated_at) SELECT ?,'title:'||i,i,'t',1 FROM n", [LIST]).await?;
    assert!(c.execute("INSERT INTO import_list_items(list_id,key,seq,title,updated_at) VALUES(?,'title:extra',1,'t',1)", [LIST]).await.is_err());
    // Provenance: needs exactly one target of the right domain; survives the list, goes with the item.
    c.execute(
        "INSERT INTO series(id,title,path,tvdb_id)VALUES(1,'S','/s',1)",
        (),
    )
    .await?;
    assert!(c.execute("INSERT INTO import_list_additions(media_type,movie_id,list_name,external_id,added_at) VALUES('tv',1,'x',1,1)", ()).await.is_err());
    c.execute("INSERT INTO import_list_additions(media_type,series_id,list_id,list_name,external_id,added_at) VALUES('tv',1,?,'A',1,1)", [LIST]).await?;
    c.execute("DELETE FROM import_lists WHERE id=?", [LIST])
        .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM import_list_additions WHERE list_id IS NULL AND list_name='A'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM import_list_runs").await
            + scalar(&c, "SELECT count(*) FROM import_list_items").await,
        0,
        "runs and snapshot go with the list"
    );
    c.execute("DELETE FROM series_tags", ()).await?;
    c.execute("DELETE FROM library_settings", ()).await.ok();
    c.execute("DELETE FROM series WHERE id=1", ()).await.ok();
    Ok(())
}
