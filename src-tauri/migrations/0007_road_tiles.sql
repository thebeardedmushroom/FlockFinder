-- Migration 0007: cached road network tiles for the camera-free route search (roadnet.rs).
-- Each row is one 0.1° tile of drivable OSM ways, gzip-compressed; refetched after 30 days.

CREATE TABLE IF NOT EXISTS road_tiles (
  tile       TEXT    PRIMARY KEY,   -- "row:col" of the 0.1° grid
  fetched_at INTEGER NOT NULL,
  data       BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_road_tiles_fetched ON road_tiles(fetched_at);
