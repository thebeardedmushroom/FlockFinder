-- Flock Finder schema, migration 0001.
-- Never edit this file after it has shipped; add a new numbered migration instead.

CREATE TABLE IF NOT EXISTS cameras (
  osm_type     TEXT    NOT NULL,
  osm_id       INTEGER NOT NULL,
  lat          REAL    NOT NULL,
  lon          REAL    NOT NULL,
  category     TEXT    NOT NULL,            -- flock | alpr | unknown
  tags_json    TEXT    NOT NULL DEFAULT '{}',
  first_seen   INTEGER NOT NULL,
  last_seen    INTEGER NOT NULL,
  stale_since  INTEGER NULL,
  PRIMARY KEY (osm_type, osm_id)
);
CREATE INDEX IF NOT EXISTS idx_cameras_lat_lon ON cameras(lat, lon);

CREATE TABLE IF NOT EXISTS grid_cells (
  cell_key      TEXT    PRIMARY KEY,
  fetched_at    INTEGER NOT NULL,
  element_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS submissions (
  id             INTEGER PRIMARY KEY,
  lat            REAL    NOT NULL,
  lon            REAL    NOT NULL,
  category       TEXT    NOT NULL,          -- flock | alpr | unsure
  direction      INTEGER NULL,
  mount          TEXT    NULL,
  operator       TEXT    NULL,
  notes          TEXT    NULL,              -- private, never uploaded
  status         TEXT    NOT NULL DEFAULT 'local',  -- local | uploaded
  osm_element_id INTEGER NULL,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_submissions_status ON submissions(status);

CREATE TABLE IF NOT EXISTS watch_areas (
  id           INTEGER PRIMARY KEY,
  name         TEXT    NOT NULL,
  lat          REAL    NOT NULL,
  lon          REAL    NOT NULL,
  radius_m     INTEGER NOT NULL,
  created_at   INTEGER NOT NULL,
  last_checked INTEGER NULL
);

CREATE TABLE IF NOT EXISTS routes (
  id           INTEGER PRIMARY KEY,
  name         TEXT    NOT NULL,
  geojson      TEXT    NOT NULL,
  corridor_m   INTEGER NOT NULL,
  created_at   INTEGER NOT NULL,
  last_checked INTEGER NULL
);

CREATE TABLE IF NOT EXISTS alert_events (
  id          INTEGER PRIMARY KEY,
  target_type TEXT    NOT NULL,             -- area | route
  target_id   INTEGER NOT NULL,
  osm_type    TEXT    NOT NULL,
  osm_id      INTEGER NOT NULL,
  event       TEXT    NOT NULL,             -- baseline | added | removed
  occurred_at INTEGER NOT NULL,
  notified    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_alert_events_target ON alert_events(target_type, target_id, occurred_at);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
