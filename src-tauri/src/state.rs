//! Shared application state managed by Tauri.

use crate::http::HttpClient;
use crate::sync::Progress;
use rusqlite::Connection;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard};

pub struct AppState {
    pub db: Mutex<Connection>,
    pub http: HttpClient,
    /// PKCE state for an OSM sign-in that is waiting for its deep-link callback.
    pub oauth_pending: Mutex<Option<crate::osm::PendingAuth>>,
    pub refresh_running: AtomicBool,
    /// The worldwide camera sync that is running right now, if any.
    pub sync: Mutex<Progress>,
}

impl AppState {
    pub fn new(db: Connection, http: HttpClient) -> Self {
        AppState {
            db: Mutex::new(db),
            http,
            oauth_pending: Mutex::new(None),
            refresh_running: AtomicBool::new(false),
            sync: Mutex::new(Progress::default()),
        }
    }

    /// Lock the database. Never hold the guard across an `.await`.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Lock the sync progress. Never hold the guard across an `.await`.
    pub fn sync_progress(&self) -> MutexGuard<'_, Progress> {
        self.sync.lock().unwrap_or_else(|p| p.into_inner())
    }
}
