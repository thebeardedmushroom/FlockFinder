-- Migration 0002: supporting tables that are implementation details of the app.

-- Nominatim results cached by the exact query string (etiquette requirement).
CREATE TABLE IF NOT EXISTS geocode_cache (
  query       TEXT    PRIMARY KEY,
  result_json TEXT    NOT NULL,
  fetched_at  INTEGER NOT NULL
);

-- One row per background alert refresh attempt, including skipped/offline ones.
CREATE TABLE IF NOT EXISTS refresh_log (
  id          INTEGER PRIMARY KEY,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER NULL,
  outcome     TEXT    NOT NULL,             -- ok | skipped_offline | error
  detail      TEXT    NULL,
  cells       INTEGER NOT NULL DEFAULT 0
);
