use hrrdarr::{
    db::{Database, Error as TestError},
    library::refresh::{self, CapturedTarget, Details, Target},
    metadata::{EpisodeDetails, MovieDetails, SeriesDetails},
};
use libsql::{Connection, Value};
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
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
async fn facts(c: &Connection) -> Vec<Vec<Value>> {
    let mut rows=c.query("SELECT id,series_id,tvdb_id,season,number,title,monitored,episode_file_id,runtime,overview FROM episodes ORDER BY id",()).await.unwrap();
    let mut values = vec![];
    while let Some(row) = rows.next().await.unwrap() {
        values.push(
            (0..row.column_count())
                .map(|i| row.get_value(i).unwrap())
                .collect(),
        );
    }
    values
}
fn episode(id: i64, season: i64, number: i64, title: &str) -> EpisodeDetails {
    EpisodeDetails {
        tvdb_id: id,
        season,
        number,
        title: title.into(),
        air_date: None,
        air_date_utc: None,
        absolute_episode_number: None,
        runtime: None,
        overview: None,
        finale_type: None,
    }
}
fn series() -> SeriesDetails {
    SeriesDetails {
        network: None,
        original_country: None,
        status: None,
        genres: None,
        original_language: None,
        tvdb_id: 101,
        title: "New TV".into(),
        year: None,
        imdb_id: None,
        seasons: vec![0, 1, 2],
        episodes: vec![
            episode(501, 1, 1, "Updated"),
            episode(502, 1, 2, "Two"),
            episode(503, 1, 3, "Three"),
            episode(504, 2, 1, "Four"),
            episode(505, 1, 0, "Season one zero"),
            episode(506, 2, 0, "Other zero"),
            episode(507, 0, 1, "Special"),
        ],
    }
}
fn movie() -> MovieDetails {
    MovieDetails {
        studio: None,
        genres: None,
        keywords: None,
        tmdb_id: 101,
        title: "New Movie".into(),
        year: None,
        imdb_id: None,
        runtime: None,
        status: None,
        in_cinemas: None,
        digital_release: None,
        physical_release: None,
        secondary_year: None,
        original_language: None,
        alternative_titles: None,
    }
}
async fn apply(
    c: &Connection,
    target: &CapturedTarget,
    details: Details,
) -> Result<u16, refresh::Error> {
    let tx = c.transaction().await.map_err(refresh::Error::from)?;
    match refresh::apply(&tx, target, details).await {
        Ok(count) => {
            tx.commit().await?;
            Ok(count)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
#[tokio::test]
async fn metadata_reconciliation_preserves_local_identity_policy_and_files() -> Result<(), TestError>
{
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-refresh-writer-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'tv','Local');
 INSERT INTO series(id,tvdb_id,title,year,path,monitored) VALUES(1,101,'Old TV',2020,'/tv/local',0);
 INSERT INTO seasons(series_id,number,monitored) VALUES(1,1,0);
 INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,season_folder,use_scene_numbering,monitor_new_items) VALUES('tv',1,1,'anime',0,1,'none');
 INSERT INTO episode_files(id,series_id,path) VALUES(7,1,'/tv/local/shared.mkv');
 INSERT INTO episodes(id,series_id,tvdb_id,season,number,title,monitored,episode_file_id,runtime,overview) VALUES(10,1,501,1,1,'One',1,7,50,'Keep overview'),(11,1,502,1,2,'Two',0,7,NULL,NULL);
 INSERT INTO movie_metadata(id,tmdb_id,title,year,imdb_id) VALUES(7,101,'Old Movie',2019,'tt101');
 INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,7,'/movies/local',0);
 INSERT INTO library_settings(media_type,movie_id,minimum_availability) VALUES('movies',1,'announced');
 INSERT INTO movie_files(id,movie_id,path) VALUES(8,1,'/movies/local/movie.mkv');").await?;
    let tv = refresh::capture(&c, Target::Tv { series_id: 1 }).await?;
    let film = refresh::capture(&c, Target::Movies { movie_id: 1 }).await?;
    assert_eq!(tv.external_id, film.external_id);
    assert_ne!(tv.target, film.target);
    assert_eq!(apply(&c, &tv, Details::Series(series())).await?, 9);
    assert_eq!(apply(&c, &tv, Details::Series(series())).await?, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE id IN(10,11) AND episode_file_id=7"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT monitored FROM episodes WHERE id=10").await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT runtime FROM episodes WHERE id=10").await,
        50
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT quality_profile_id FROM library_settings WHERE series_id=1"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE tvdb_id>=503 AND monitored=0"
        )
        .await,
        5
    );
    assert_eq!(scalar(&c, "SELECT year FROM series WHERE id=1").await, 2020);
    assert_eq!(apply(&c, &film, Details::Movie(movie())).await?, 1);
    assert_eq!(apply(&c, &film, Details::Movie(movie())).await?, 0);
    assert_eq!(scalar(&c,"SELECT count(*) FROM movie_files WHERE id=8 AND movie_id=1 AND path='/movies/local/movie.mkv'").await,1);
    assert_eq!(
        scalar(&c, "SELECT year FROM movie_metadata WHERE id=7").await,
        2019
    );
    // Direct Rust DTOs receive full-document preflight too: a late bad episode
    // or fact cannot update title/network first, even before caller rollback.
    for kind in 0..5 {
        let mut incoming = series();
        incoming.title = "Must not write".into();
        incoming.network = Some("Must not write".into());
        match kind {
            0 => incoming.genres = Some(vec!["valid".into(), "bad\0".into()]),
            1 => incoming.original_country = Some("US".into()),
            2 => incoming.episodes.last_mut().unwrap().title = "".into(),
            3 => incoming.episodes.last_mut().unwrap().air_date = Some("".into()),
            _ => incoming.episodes.last_mut().unwrap().air_date_utc = Some("invalid".into()),
        }
        let tx = c.transaction().await?;
        assert_eq!(
            refresh::apply(&tx, &tv, Details::Series(incoming)).await,
            Err(refresh::Error::Conflict)
        );
        assert_eq!(
            scalar(
                &tx,
                "SELECT count(*) FROM series WHERE title='New TV' AND network IS NULL"
            )
            .await,
            1
        );
        tx.rollback().await?;
    }
    for kind in 0..5 {
        let mut incoming = movie();
        incoming.title = "Must not write".into();
        incoming.studio = Some("Must not write".into());
        match kind {
            0 => incoming.keywords = Some(vec!["valid".into(), "bad\0".into()]),
            1 => incoming.runtime = Some(-1),
            2 => incoming.status = Some("Unknown enum".into()),
            3 => incoming.digital_release = Some("".into()),
            _ => incoming.alternative_titles = Some(vec!["".into()]),
        }
        let tx = c.transaction().await?;
        assert_eq!(
            refresh::apply(&tx, &film, Details::Movie(incoming)).await,
            Err(refresh::Error::Conflict)
        );
        assert_eq!(
            scalar(
                &tx,
                "SELECT count(*) FROM movie_metadata WHERE title='New Movie' AND studio IS NULL"
            )
            .await,
            1
        );
        tx.rollback().await?;
    }
    let before = facts(&c).await;
    for kind in 0..3 {
        let mut incoming = series();
        match kind {
            0 => {
                incoming.episodes.remove(0);
            }
            1 => {
                incoming.episodes[0].number = 99;
            }
            _ => {
                incoming.seasons.remove(0);
                incoming.episodes.retain(|e| e.season != 0);
            }
        }
        assert_eq!(
            apply(&c, &tv, Details::Series(incoming)).await,
            Err(refresh::Error::Conflict)
        );
        assert_eq!(facts(&c).await, before);
    }
    for sql in [
        "UPDATE episodes SET tvdb_id=NULL WHERE id=10",
        "INSERT INTO series(id,tvdb_id,title,path) VALUES(2,202,'Other','/other');INSERT INTO seasons VALUES(2,1,1);INSERT INTO episodes(series_id,tvdb_id,season,number,title) VALUES(2,501,1,1,'Collision')",
        "UPDATE episodes SET tvdb_id=501 WHERE id=11",
    ] {
        let tx = c.transaction().await?;
        tx.execute_batch(sql).await?;
        assert_eq!(
            refresh::apply(&tx, &tv, Details::Series(series())).await,
            Err(refresh::Error::Conflict)
        );
        tx.rollback().await?;
        assert_eq!(facts(&c).await, before);
    }
    let tx = c.transaction().await?;
    tx.execute("UPDATE series SET tvdb_id=999 WHERE id=1", ())
        .await?;
    assert_eq!(
        refresh::apply(&tx, &tv, Details::Series(series())).await,
        Err(refresh::Error::TargetChanged)
    );
    tx.rollback().await?;
    let tx = c.transaction().await?;
    tx.execute(
        "INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(98,999,'Replacement')",
        (),
    )
    .await?;
    tx.execute("UPDATE movies SET metadata_id=98 WHERE id=1", ())
        .await?;
    assert_eq!(
        refresh::apply(&tx, &film, Details::Movie(movie())).await,
        Err(refresh::Error::TargetChanged)
    );
    tx.rollback().await?;
    c.execute(
        "INSERT INTO movie_metadata(id,title,imdb_id) VALUES(99,'Other','tt0000999')",
        (),
    )
    .await?;
    let mut incoming = movie();
    // Valid IMDb syntax keeps this a duplicate-identity conflict, not input validation.
    incoming.imdb_id = Some("tt0000999".into());
    assert_eq!(
        apply(&c, &film, Details::Movie(incoming)).await,
        Err(refresh::Error::Conflict)
    );
    // A late episode insert failure occurs after catalog updates; the caller rolls all of them back.
    c.execute_batch("CREATE TRIGGER late_refresh BEFORE INSERT ON episodes WHEN NEW.tvdb_id=508 BEGIN SELECT RAISE(ABORT,'fixture');END;").await?;
    let mut incoming = series();
    incoming.title = "Must roll back".into();
    incoming.episodes.push(episode(508, 2, 2, "New"));
    assert_eq!(
        apply(&c, &tv, Details::Series(incoming)).await,
        Err(refresh::Error::Storage)
    );
    assert_eq!(facts(&c).await, before);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE title='New TV'").await,
        1
    );
    c.execute("DROP TRIGGER late_refresh", ()).await?;
    c.execute(
        "UPDATE library_settings SET monitor_new_items='all' WHERE series_id=1",
        (),
    )
    .await?;
    let mut incoming = series();
    incoming.seasons.push(3);
    incoming.episodes.push(episode(508, 3, 1, "New monitored"));
    incoming.episodes.push(episode(509, 3, 0, "New zero"));
    assert_eq!(apply(&c, &tv, Details::Series(incoming.clone())).await?, 3);
    assert_eq!(
        scalar(&c, "SELECT monitored FROM episodes WHERE tvdb_id=508").await,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT monitored FROM episodes WHERE tvdb_id=509").await,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT monitored FROM series WHERE id=1").await,
        0
    );
    // Unknown monitoring policy is a visible conflict only when a new non-special season is needed.
    c.execute(
        "UPDATE library_settings SET monitor_new_items=NULL WHERE series_id=1",
        (),
    )
    .await?;
    incoming.seasons.push(4);
    assert_eq!(
        apply(&c, &tv, Details::Series(incoming)).await,
        Err(refresh::Error::Conflict)
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM episodes WHERE episode_file_id=7").await,
        2
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM library_settings WHERE series_type='anime' AND use_scene_numbering=1 AND quality_profile_id=1").await,1);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movies WHERE path='/movies/local' AND monitored=0"
        )
        .await,
        1
    );
    Ok(())
}
