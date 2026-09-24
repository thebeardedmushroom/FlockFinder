-- Migration 0004: Wi-Fi fingerprint sightings (suspected Flock devices inferred from
-- Wi-Fi OUI matches). Sources: the published Flock Finder (simeononsecurity) dataset
-- built from WiGLE, and the user's own Wigle-format wardriving CSV imports.
-- Kept strictly apart from `cameras` (OSM): heuristic, "suspected" data only.

CREATE TABLE IF NOT EXISTS wifi_sightings (
  netid       TEXT    PRIMARY KEY,          -- BSSID / MAC, normalised AA:BB:CC:DD:EE:FF
  lat         REAL    NOT NULL,
  lon         REAL    NOT NULL,
  oui         TEXT    NOT NULL,              -- matched 3-octet prefix
  ssid        TEXT    NULL,
  channel     INTEGER NULL,
  encryption  TEXT    NULL,
  first_seen  TEXT    NULL,                  -- as reported by the source (ISO-ish text)
  last_seen   TEXT    NULL,
  city        TEXT    NULL,
  region      TEXT    NULL,
  country     TEXT    NULL,
  road        TEXT    NULL,
  postalcode  TEXT    NULL,
  source      TEXT    NOT NULL,              -- upstream | wigle_import
  imported_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wifi_sightings_lat_lon ON wifi_sightings(lat, lon);
CREATE INDEX IF NOT EXISTS idx_wifi_sightings_source ON wifi_sightings(source);
