//! hc.034 root-folder and hc.035 read-only mount detectors against owned temporary folders.
//! Mount tables are injected; only the dedicated Linux unit test reads /proc/self/mountinfo.
use super::*;
use crate::health::{library_roots, mounts};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn identity(scope: HealthScope, key: &str) -> HealthIdentity {
    HealthIdentity {
        scope,
        check_key: key.into(),
    }
}

/// Library folders live in their own owned directory, canonicalized for mount-table paths.
fn library(scratch: &Scratch) -> PathBuf {
    let root = scratch.0.join("library");
    std::fs::create_dir(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

async fn roots(db: &Database, scope: HealthScope, key: &str) -> Option<HealthIssue> {
    library_roots::evaluate(db, &identity(scope, key))
        .await
        .unwrap()
}

/// Every entry (path, kind, size, mtime) below `root`, recursively, without following links.
fn tree(root: &Path) -> Vec<(PathBuf, bool, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.push((
                entry.path(),
                meta.is_dir(),
                meta.len(),
                meta.modified().unwrap(),
            ));
        }
    }
    out.sort();
    out
}

struct Fixed(std::result::Result<String, std::io::ErrorKind>);
impl mounts::MountSource for Fixed {
    fn read(&self, _: usize) -> std::io::Result<Vec<u8>> {
        self.0
            .clone()
            .map(String::into_bytes)
            .map_err(std::io::Error::from)
    }
}

/// proc(5) octal escaping for the injected mount table.
fn escape(path: &Path) -> String {
    text(path)
        .replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
}

fn table(read_only: &[&Path], writable: &[&Path]) -> Arc<dyn mounts::MountSource> {
    let mut lines = vec!["20 1 259:2 / / rw,relatime shared:1 - ext4 /dev/root rw".to_owned()];
    for (n, path) in writable.iter().enumerate() {
        lines.push(format!(
            "{} 20 0:{} / {} rw,relatime shared:{} - tmpfs tmpfs rw",
            30 + n,
            30 + n,
            escape(path),
            30 + n
        ));
    }
    for (n, path) in read_only.iter().enumerate() {
        // A read-only bind mount: per-mount ro over a writable superblock, non-root source.
        lines.push(format!(
            "{} 20 259:2 /srv/bind{n} {} ro,relatime shared:{} - ext4 /dev/root rw",
            60 + n,
            escape(path),
            60 + n
        ));
    }
    Arc::new(Fixed(Ok(lines.join("\n") + "\n")))
}

async fn mount_issue(
    db: &Database,
    scope: HealthScope,
    source: Arc<dyn mounts::MountSource>,
) -> Option<HealthIssue> {
    mounts::evaluate_with(
        db,
        &identity(scope, "mount_read_only"),
        source,
        &library_roots::Limits::DEFAULT,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn missing_roots_aggregate_per_domain_restore_and_never_create() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    let (tv, movies, gone) = (lib.join("tv"), lib.join("movies"), lib.join("gone"));
    std::fs::create_dir(&tv).unwrap();
    std::fs::create_dir(&movies).unwrap();
    // Colliding numeric IDs across domains: series 1..3, movies 1..2.
    c.execute_batch(&format!(
        "INSERT INTO root_folders(media_type,path) VALUES('tv','{tv}'),('movies','{movies}');
         INSERT INTO series(id,title,path) VALUES(1,'Alpha','{tv}/Alpha'),(2,'Beta','{tv}/Beta'),(3,'Gamma','{gone}/Gamma');
         INSERT INTO movie_metadata(id,title) VALUES(1,'Film One'),(2,'Film Two');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{movies}/One'),(2,2,'{movies}/Two');",
        tv = text(&tv),
        movies = text(&movies),
        gone = text(&gone)
    ))
    .await
    .unwrap();
    // Series 3 has no configured root: upstream derives its parent folder, which is missing.
    let issue = roots(&db, HealthScope::Tv, "root_folder").await.unwrap();
    assert_eq!(issue.severity, HealthSeverity::Error);
    assert_eq!(issue.reason, "root_folder_missing");
    assert_eq!(issue.compatibility_type, "RootFolderCheck");
    assert_eq!(
        issue.wiki_url,
        "https://wiki.servarr.com/sonarr/system#missing-root-folder"
    );
    assert!(issue.message.starts_with(&format!(
        "Missing root folder: {} (not found; 1 series, e.g. Gamma)",
        text(&gone)
    )));
    assert!(issue.message.contains("never creates"));
    assert!(
        roots(&db, HealthScope::Movies, "root_folder")
            .await
            .is_none()
    );
    // Two movies under one missing root aggregate into one entry; TV is unaffected by movie IDs.
    let before = tree(&lib);
    std::fs::remove_dir(&movies).unwrap();
    let issue = roots(&db, HealthScope::Movies, "root_folder")
        .await
        .unwrap();
    assert_eq!(issue.reason, "root_folder_missing");
    assert_eq!(
        issue.wiki_url,
        "https://wiki.servarr.com/radarr/system#missing-root-folder"
    );
    assert!(issue.message.starts_with(&format!(
        "Missing root folder: {} (not found; 2 movies, e.g. Film One, Film Two)",
        text(&movies)
    )));
    let tv_issue = roots(&db, HealthScope::Tv, "root_folder").await.unwrap();
    assert!(!tv_issue.message.contains("Film"));
    assert!(!movies.exists(), "evaluation must never create a root");
    // Two missing TV roots produce the upstream multiple-missing aggregate.
    std::fs::rename(&tv, lib.join("tv-moved")).unwrap();
    let issue = roots(&db, HealthScope::Tv, "root_folder").await.unwrap();
    assert_eq!(issue.reason, "root_folder_multiple_missing");
    assert!(
        issue
            .message
            .starts_with("Multiple root folders are missing: ")
    );
    assert!(
        issue
            .message
            .contains("(not found; 2 series, e.g. Alpha, Beta)")
    );
    assert!(issue.message.contains(" | "));
    // Restored roots clear both domains.
    std::fs::rename(lib.join("tv-moved"), &tv).unwrap();
    std::fs::create_dir(&gone).unwrap();
    std::fs::create_dir(&movies).unwrap();
    assert!(roots(&db, HealthScope::Tv, "root_folder").await.is_none());
    assert!(
        roots(&db, HealthScope::Movies, "root_folder")
            .await
            .is_none()
    );
    // Evaluation created nothing: the same entries exist as before the test changed folders.
    std::fs::remove_dir(&gone).unwrap();
    let names = |entries: Vec<(PathBuf, bool, u64, std::time::SystemTime)>| {
        entries
            .into_iter()
            .map(|(path, dir, ..)| (path, dir))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(tree(&lib)), names(before));
}

#[tokio::test]
async fn root_states_distinguish_file_symlink_permission_and_invalid() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    let real = lib.join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(lib.join("file"), b"x").unwrap();
    std::os::unix::fs::symlink(&real, lib.join("link")).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'File','{lib}/file/Show'),(2,'Link','{lib}/link/Show'),(3,'Relative','relative/Show')",
        lib = text(&lib)
    ))
    .await
    .unwrap();
    let issue = roots(&db, HealthScope::Tv, "root_folder").await.unwrap();
    assert_eq!(issue.reason, "root_folder_multiple_missing");
    assert!(
        issue
            .message
            .contains(&format!("{}/file (not a folder; 1 series", text(&lib)))
    );
    assert!(issue.message.contains(&format!(
        "{}/link (path traverses a symbolic link, which hrrdarr refuses; 1 series",
        text(&lib)
    )));
    assert!(
        issue
            .message
            .contains("(invalid library paths) (invalid library path; 1 series, e.g. Relative)")
    );
    // Permission denial is only observable without CAP_DAC_OVERRIDE.
    if !rustix::process::geteuid().is_root() {
        use std::os::unix::fs::PermissionsExt;
        let locked = lib.join("locked");
        std::fs::create_dir_all(locked.join("tv")).unwrap();
        c.execute_batch(&format!(
            "DELETE FROM series; INSERT INTO series(id,title,path) VALUES(1,'Locked','{}/tv/Show')",
            text(&locked)
        ))
        .await
        .unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let issue = roots(&db, HealthScope::Tv, "root_folder").await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let issue = issue.unwrap();
        assert_eq!(issue.reason, "root_folder_permission_denied");
        assert!(issue.message.contains("(permission denied; 1 series"));
    }
}

#[tokio::test]
async fn collection_root_is_a_separate_movies_identity() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    let (movies, saga) = (lib.join("movies"), lib.join("saga"));
    std::fs::create_dir(&movies).unwrap();
    std::fs::create_dir(&saga).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO root_folders(id,media_type,path) VALUES(1,'movies','{movies}'),(2,'movies','{saga}'),(3,'tv','{lib}/tvroot');
         INSERT INTO series(id,title,path) VALUES(1,'Series One','{lib}/Series');
         INSERT INTO movie_metadata(id,title) VALUES(1,'Film One');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{movies}/One');
         INSERT INTO movie_collections(id,tmdb_id,title,root_folder_id) VALUES(1,10,'Saga A',2),(2,11,'Saga B',2),(3,12,'Unrooted',NULL);",
        movies = text(&movies),
        saga = text(&saga),
        lib = text(&lib)
    ))
    .await
    .unwrap();
    for (scope, key) in [
        (HealthScope::Tv, "root_folder"),
        (HealthScope::Movies, "root_folder"),
        (HealthScope::Movies, "collection_root_folder"),
    ] {
        assert!(roots(&db, scope, key).await.is_none(), "{key}");
    }
    std::fs::remove_dir(&saga).unwrap();
    let issue = roots(&db, HealthScope::Movies, "collection_root_folder")
        .await
        .unwrap();
    assert_eq!(issue.severity, HealthSeverity::Error);
    assert_eq!(issue.reason, "collection_root_folder_missing");
    assert_eq!(issue.compatibility_type, "MovieCollectionRootFolderCheck");
    assert_eq!(
        issue.wiki_url,
        "https://wiki.servarr.com/radarr/system#movie-collection-missing-root-folder"
    );
    assert!(issue.message.starts_with(&format!(
        "Missing root folder for movie collection: {} (not found; 2 collections, e.g. Saga A, Saga B)",
        text(&saga)
    )));
    assert!(!issue.message.contains("Unrooted"));
    // The movie root and TV are separate identities and remain healthy.
    assert!(
        roots(&db, HealthScope::Movies, "root_folder")
            .await
            .is_none()
    );
    assert!(roots(&db, HealthScope::Tv, "root_folder").await.is_none());
    // Collections are not a TV identity.
    assert_eq!(
        library_roots::evaluate(&db, &identity(HealthScope::Tv, "collection_root_folder"))
            .await
            .unwrap_err(),
        "check_failed"
    );
    std::fs::create_dir(&saga).unwrap();
    assert!(
        roots(&db, HealthScope::Movies, "collection_root_folder")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn root_bounds_and_time_budget_are_reported_not_healthy() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    for name in ["a", "b"] {
        std::fs::create_dir(lib.join(name)).unwrap();
    }
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'One','{lib}/a/One'),(2,'Two','{lib}/b/Two'),(3,'Three','{lib}/missing/Three')",
        lib = text(&lib)
    ))
    .await
    .unwrap();
    let id = identity(HealthScope::Tv, "root_folder");
    let limits = |items, groups, budget| library_roots::Limits {
        items,
        item_bytes: 1 << 20,
        groups,
        budget,
    };
    // Item bound: the missing third root is never scanned, so the result is an explicit Notice.
    let issue = library_roots::evaluate_with(&db, &id, &limits(2, 16, Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(issue.severity, HealthSeverity::Notice);
    assert_eq!(issue.reason, "root_folder_check_truncated");
    assert!(issue.message.contains("1 series were not evaluated"));
    // Distinct-root bound: only the first root is probed; two series stay unevaluated.
    let issue = library_roots::evaluate_with(&db, &id, &limits(100, 1, Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(issue.reason, "root_folder_check_truncated");
    assert!(issue.message.contains("2 series were not evaluated"));
    // Exhausted time budget: nothing was probed, nothing is claimed healthy.
    let issue = library_roots::evaluate_with(&db, &id, &limits(100, 16, Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(issue.severity, HealthSeverity::Notice);
    assert!(issue.message.contains("3 series were not evaluated"));
    // Within bounds the missing root is an Error, and a bounded scan appends its truncation.
    let full = library_roots::evaluate_with(&db, &id, &limits(100, 16, Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(full.reason, "root_folder_missing");
    assert!(!full.message.contains("not evaluated"));
    c.execute(
        "UPDATE series SET path=? WHERE id=1",
        [format!("{}/missing/One", text(&lib))],
    )
    .await
    .unwrap();
    let bounded = library_roots::evaluate_with(&db, &id, &limits(1, 16, Duration::from_secs(2)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bounded.reason, "root_folder_missing");
    assert!(bounded.message.contains("1 series, e.g. One"));
    assert!(bounded.message.contains("2 series were not evaluated"));
    // Many missing roots: the message lists five and counts the rest, within the stored bound.
    let long = "x".repeat(400);
    for n in 10..40 {
        c.execute(
            "INSERT INTO series(id,title,path) VALUES(?,?,?)",
            params![n, long.clone(), format!("{}/gone{n}-{long}/S", text(&lib))],
        )
        .await
        .unwrap();
    }
    let many = library_roots::evaluate(&db, &id).await.unwrap().unwrap();
    assert_eq!(many.reason, "root_folder_multiple_missing");
    assert!(many.message.contains("more root folders not listed"));
    assert!(many.message.len() <= 4096);
}

#[tokio::test]
async fn read_only_mounts_are_detected_per_domain_from_an_injected_table() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    let (ro, rw) = (lib.join("ro media"), lib.join("rw"));
    for dir in [
        ro.join("Show"),
        ro.join("Second"),
        ro.join("Film"),
        rw.join("Film"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::os::unix::fs::symlink(ro.join("Second"), rw.join("Linked")).unwrap();
    // Colliding IDs: series 1..3 and movie 1..2.
    c.execute_batch(&format!(
        "INSERT INTO series(id,title,path) VALUES(1,'Show','{ro}/Show'),(2,'Linked','{rw}/Linked'),(3,'Absent','{ro}/Absent');
         INSERT INTO movie_metadata(id,title) VALUES(1,'Writable Film'),(2,'Read-only Film');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{rw}/Film');",
        ro = text(&ro),
        rw = text(&rw)
    ))
    .await
    .unwrap();
    let before = tree(&lib);
    let source = table(&[&ro], &[&rw]);
    let issue = mount_issue(&db, HealthScope::Tv, source.clone())
        .await
        .unwrap();
    assert_eq!(issue.severity, HealthSeverity::Error);
    assert_eq!(issue.reason, "mount_read_only");
    assert_eq!(issue.compatibility_type, "MountCheck");
    assert_eq!(
        issue.wiki_url,
        "https://wiki.servarr.com/sonarr/system#series-mount-ro"
    );
    // The symlinked item resolves into the read-only mount; the absent folder is not counted.
    assert!(issue.message.starts_with(&format!(
        "Mount containing a series path is mounted read-only: {} (ext4; 2 series, e.g. Show, Linked)",
        text(&ro)
    )));
    assert!(!issue.message.contains("Absent"));
    assert!(
        mount_issue(&db, HealthScope::Movies, source.clone())
            .await
            .is_none()
    );
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(2,2,?)",
        [format!("{}/Film", text(&ro))],
    )
    .await
    .unwrap();
    let issue = mount_issue(&db, HealthScope::Movies, source.clone())
        .await
        .unwrap();
    assert_eq!(
        issue.wiki_url,
        "https://wiki.servarr.com/radarr/system#movies-mount-ro"
    );
    assert!(issue.message.starts_with(&format!(
        "Mount containing a movie path is mounted read-only: {} (ext4; 1 movie, e.g. Read-only Film)",
        text(&ro)
    )));
    // Remounted read-write clears both domains.
    let remounted = table(&[], &[&rw, &ro]);
    assert!(
        mount_issue(&db, HealthScope::Tv, remounted.clone())
            .await
            .is_none()
    );
    assert!(
        mount_issue(&db, HealthScope::Movies, remounted)
            .await
            .is_none()
    );
    // An unreadable or unsupported table degrades to an explicit Notice, never silent health.
    for (kind, text) in [
        (std::io::ErrorKind::PermissionDenied, "could not be read"),
        (
            std::io::ErrorKind::Unsupported,
            "not supported on this platform",
        ),
    ] {
        let issue = mount_issue(&db, HealthScope::Tv, Arc::new(Fixed(Err(kind))))
            .await
            .unwrap();
        assert_eq!(issue.severity, HealthSeverity::Notice);
        assert_eq!(issue.reason, "mount_table_unavailable");
        assert!(issue.message.contains(text));
    }
    let garbage = mount_issue(&db, HealthScope::Tv, Arc::new(Fixed(Ok("junk\n".into()))))
        .await
        .unwrap();
    assert!(garbage.message.contains("could not be parsed"));
    // Time budget exhausted: an explicit Notice, not a healthy observation.
    let truncated = mounts::evaluate_with(
        &db,
        &identity(HealthScope::Tv, "mount_read_only"),
        source,
        &library_roots::Limits {
            budget: Duration::ZERO,
            ..library_roots::Limits::DEFAULT
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(truncated.severity, HealthSeverity::Notice);
    assert_eq!(truncated.reason, "mount_check_truncated");
    assert!(truncated.message.contains("3 series were not evaluated"));
    assert_eq!(tree(&lib), before, "mount evaluation must not write");
}

async fn outcomes(
    db: &Database,
    cmd: &HealthCommand,
    source: Arc<dyn mounts::MountSource>,
) -> Vec<CheckOutcome> {
    let mut out = Vec::new();
    for member in &cmd.members {
        out.push(match member.identity.check_key.as_str() {
            "root_folder" | "collection_root_folder" => {
                library_roots::evaluate(db, &member.identity).await
            }
            "mount_read_only" => {
                mounts::evaluate_with(
                    db,
                    &member.identity,
                    source.clone(),
                    &library_roots::Limits::DEFAULT,
                )
                .await
            }
            _ => Ok(None),
        });
    }
    out
}

#[tokio::test]
async fn library_root_issue_publishes_and_restores_through_the_engine() {
    let (scratch, db, c) = fixture_all().await;
    let lib = library(&scratch);
    let (tv, ro) = (lib.join("tv"), lib.join("ro"));
    std::fs::create_dir_all(ro.join("Show")).unwrap();
    c.execute_batch(&format!(
        "INSERT INTO root_folders(media_type,path) VALUES('tv','{tv}');
         INSERT INTO series(id,title,path) VALUES(1,'Rooted','{tv}/Rooted'),(2,'Mounted','{ro}/Show');
         INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'{tv}/Rooted');",
        tv = text(&tv),
        ro = text(&ro)
    ))
    .await
    .unwrap();
    let source = table(&[&ro], &[]);
    init(&c, 100).await;
    let cmd = take(&c, admit(&c, HealthScope::All, 100).await, 100).await;
    // Reasoning: 0077 registers root_folder (both), collection_root_folder (movies) and
    // mount_read_only (both); with the 24 earlier identities every registry member is selected.
    assert_eq!(cmd.members.len(), 29);
    let results = outcomes(&db, &cmd, source.clone()).await;
    publish_outcomes(&c, &cmd, results, 101).await.unwrap();
    assert!(matches!(
        read(&c, cmd.id).await.unwrap().status,
        CommandStatus::Succeeded
    ));
    // The same missing root is reported independently by TV and movies; mounts only for TV.
    assert_eq!(count(&c, "SELECT count(*) FROM health_checks WHERE check_key='root_folder' AND severity=3 AND reason='root_folder_missing'").await, 2);
    assert_eq!(count(&c, "SELECT count(*) FROM health_checks WHERE scope='tv' AND check_key='mount_read_only' AND severity=3").await, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM health_checks WHERE scope='movies' AND check_key IN ('mount_read_only','collection_root_folder') AND severity=0").await, 2);
    assert_eq!(count(&c, "SELECT count(*) FROM health_transitions WHERE kind='issue' AND compatibility_type IN ('RootFolderCheck','MountCheck')").await, 3);
    std::fs::create_dir(&tv).unwrap();
    let cmd = take(&c, admit(&c, HealthScope::All, 102).await, 102).await;
    let results = outcomes(&db, &cmd, table(&[], &[&ro])).await;
    publish_outcomes(&c, &cmd, results, 103).await.unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM health_checks WHERE check_key IN ('root_folder','mount_read_only','collection_root_folder') AND severity=0").await, 5);
    assert_eq!(count(&c, "SELECT count(*) FROM health_transitions WHERE kind='restored' AND (message LIKE 'Missing root folder: %' OR message LIKE 'Mount containing a series path%')").await, 3);
}

#[tokio::test]
async fn root_folder_mutations_mark_library_root_checks() {
    let (_scratch, _db, c) = fixture_all().await;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    library_roots_changed(&tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM health_checks WHERE pending_reasons=8 AND generation=1 AND check_key IN ('root_folder','collection_root_folder')").await, 3);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM health_checks WHERE pending_reasons!=0"
        )
        .await,
        3
    );
    // Marking outside the authoritative transaction is refused.
    assert!(library_roots_changed(&c).await.is_err());
}
