//! Import lists (Sonarr "Import Lists", Radarr "Lists"): external sources of series and movies that
//! are synchronised on a schedule or on demand and added to the library through the one safe
//! catalog add path. See `docs/import-lists.md` for the contract and its limits.
pub mod api;
pub mod catalog;
pub mod error;
pub mod exclusions;
pub mod model;
pub mod providers;
pub mod store;
pub mod sync;

use crate::{api::MediaDomain, db::Database, metadata::MetadataClient, notifications::transport::Transport, providers::CredentialKey};
use libsql::{Connection, params};
use std::sync::Arc;
use tokio::sync::Notify;

/// Everything outbound a list needs: its own bounded HTTP transport and the metadata client the
/// library add path uses.
#[derive(Clone)]
pub struct Deps {
    pub transport: Arc<Transport>,
    pub metadata: Arc<MetadataClient>,
}
impl Deps {
    pub fn production(metadata: Arc<MetadataClient>) -> Self {
        Self {
            transport: Arc::new(Transport::new().expect("static HTTP client configuration")),
            metadata,
        }
    }
}

pub fn engine(db: Arc<Database>, key: Option<Arc<CredentialKey>>, deps: Deps) -> sync::Engine {
    sync::Engine::new(Arc::new(store::Store { db, key }), deps.transport, deps.metadata)
}

/// Registry, exclusion, run and test routes only; scheduled synchronisation runs in [`start`].
pub fn router(db: Arc<Database>, key: Option<Arc<CredentialKey>>, deps: Deps) -> axum::Router {
    api::router(Arc::new(engine(db, key, deps)))
}

/// Whether the domain's exclusion list contains this external id (TVDB for TV, TMDb for movies).
pub async fn excluded(c: &Connection, media: MediaDomain, external_id: i64) -> Result<bool, libsql::Error> {
    let mut rows = c
        .query(
            "SELECT 1 FROM import_list_exclusions WHERE media_type=? AND external_id=?",
            params![store::domain_name(media), external_id],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Provenance for a record the add path just created; runs in the same transaction.
pub async fn record_addition(
    c: &Connection,
    media: MediaDomain,
    library_id: i64,
    external_id: i64,
    add: &crate::library::ListAdd,
) -> Result<(), libsql::Error> {
    let (series, movie) = match media {
        MediaDomain::Tv => (Some(library_id), None),
        MediaDomain::Movies => (None, Some(library_id)),
    };
    c.execute(
        "INSERT INTO import_list_additions(media_type,series_id,movie_id,list_id,list_name,run_id,external_id,search_requested,added_at) VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            store::domain_name(media),
            series,
            movie,
            add.list_id.clone(),
            add.list_name.clone(),
            add.run_id.clone(),
            external_id,
            i64::from(add.search_requested),
            store::now()
        ],
    )
    .await?;
    Ok(())
}

/// Handle to the background scheduler; `shutdown` stops it and waits for the current pass.
pub struct Handle {
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Handle {
    pub async fn shutdown(self) {
        self.stop.notify_one();
        let _ = self.task.await;
    }
}

/// Starts the scheduler. Only the process that owns the database should call this.
pub fn start(db: Arc<Database>, key: Option<Arc<CredentialKey>>, deps: Deps) -> Handle {
    let engine = engine(db, key, deps);
    let stop = Arc::new(Notify::new());
    let signal = stop.clone();
    let task = tokio::spawn(async move {
        loop {
            if let Err(error) = engine.tick(4).await {
                eprintln!("event=import_list_tick_failed code={}", error.code);
            }
            tokio::select! {
                () = signal.notified() => break,
                () = tokio::time::sleep(sync::TICK) => {}
            }
        }
    });
    Handle { stop, task }
}
