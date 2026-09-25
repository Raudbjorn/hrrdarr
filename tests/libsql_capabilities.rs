//! Local embedded engine capability evidence; does not exercise remote Turso.

#[tokio::test]
async fn local_migration_capabilities() -> Result<(), libsql::Error> {
    let db = libsql::Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    assert_eq!(
        conn.query("PRAGMA foreign_keys", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        1
    );

    let tx = conn.transaction().await?;
    tx.execute("CREATE TABLE parent (id INTEGER PRIMARY KEY)", ())
        .await?;
    tx.execute(
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id))",
        (),
    )
    .await?;
    tx.execute("INSERT INTO parent VALUES (1)", ()).await?;
    let mut returned = tx
        .query(
            "INSERT INTO child VALUES (7, 1) RETURNING id, parent_id",
            (),
        )
        .await?;
    let row = returned.next().await?.unwrap();
    assert_eq!((row.get::<i64>(0)?, row.get::<i64>(1)?), (7, 1));
    assert!(returned.next().await?.is_none());
    drop(returned);
    tx.commit().await?;

    for sql in [
        "INSERT INTO child VALUES (8, 999)",
        "DELETE FROM parent WHERE id = 1",
    ] {
        let error = conn.execute(sql, ()).await.unwrap_err();
        assert!(
            matches!(&error, libsql::Error::SqliteFailure(_, message) if message.contains("FOREIGN KEY constraint failed")),
            "expected foreign-key rejection, got {error:?}"
        );
    }

    let tx = conn.transaction().await?;
    tx.execute(
        "ALTER TABLE child ADD COLUMN label TEXT NOT NULL DEFAULT 'kept'",
        (),
    )
    .await?;
    tx.execute("ALTER TABLE child RENAME COLUMN label TO title", ())
        .await?;
    tx.execute("ALTER TABLE child RENAME TO renamed_child", ())
        .await?;
    tx.commit().await?;
    assert_eq!(
        conn.query("SELECT title FROM renamed_child WHERE id = 7", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "kept"
    );

    // A failed migration must roll back both DDL and preceding data writes.
    let tx = conn.transaction().await?;
    tx.execute("ALTER TABLE renamed_child DROP COLUMN title", ())
        .await?;
    tx.execute("INSERT INTO parent VALUES (2)", ()).await?;
    assert!(
        tx.execute("INSERT INTO parent VALUES (1)", ())
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        conn.query(
            "SELECT title, (SELECT count(*) FROM parent) FROM renamed_child",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<String>(0)?,
        "kept"
    );
    assert_eq!(
        conn.query("SELECT count(*) FROM parent", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        1
    );
    assert!(
        conn.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    assert_eq!(
        conn.query("PRAGMA integrity_check", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "ok"
    );
    Ok(())
}
