-- Migration 0005: worldwide camera sync.
-- The map is served entirely from the local cameras table. Every attempt to refresh that
-- table from Overpass is recorded here, so the UI can tell "never synced" from "sync failed".

CREATE TABLE IF NOT EXISTS sync_runs (
  id           INTEGER PRIMARY KEY,
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER NULL,
  outcome      TEXT    NOT NULL,             -- running | ok | error | offline
  detail       TEXT    NULL,
  elements     INTEGER NULL,
  marked_stale INTEGER NULL,
  bytes        INTEGER NULL
);
CREATE INDEX IF NOT EXISTS idx_sync_runs_started ON sync_runs(started_at);
