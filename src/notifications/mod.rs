//! Notifications (Sonarr "Connections"): registry, durable event outbox, dispatcher and
//! provider implementations. See `docs/notifications.md` for the contract and its limits.
pub mod api;
pub mod catalog;
pub mod dispatcher;
pub mod error;
pub mod events;
pub mod model;
pub mod script;
pub mod store;
pub mod transport;

use crate::{db::Database, providers::CredentialKey};
use std::sync::Arc;

/// Registry and test routes only; delivery runs in [`start`].
pub fn router(db: Arc<Database>, key: Option<Arc<CredentialKey>>) -> axum::Router {
    api::router(Arc::new(dispatcher::Dispatcher::new(
        db,
        key,
        transport::Transport::new().expect("static HTTP client configuration"),
    )))
}

/// Starts the outbox worker. Only the process that owns the database should call this.
pub fn start(db: Arc<Database>, key: Option<Arc<CredentialKey>>) -> dispatcher::Handle {
    dispatcher::start(dispatcher::Dispatcher::new(
        db,
        key,
        transport::Transport::new().expect("static HTTP client configuration"),
    ))
}
