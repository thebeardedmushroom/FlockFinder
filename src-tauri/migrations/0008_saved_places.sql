-- Migration 0008: saved places (Home, Work and up to 10 custom places) for quick navigation.
-- Coordinates are stored when the place is saved; trips use them as they are, never re-geocoding.

CREATE TABLE IF NOT EXISTS saved_places (
  id         INTEGER PRIMARY KEY,
  kind       TEXT    NOT NULL CHECK (kind IN ('home', 'work', 'custom')),
  label      TEXT    NOT NULL,
  address    TEXT    NOT NULL,
  lat        REAL    NOT NULL,
  lon        REAL    NOT NULL,
  created_at INTEGER NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0
);
-- At most one Home and one Work.
CREATE UNIQUE INDEX IF NOT EXISTS idx_saved_places_slot ON saved_places(kind) WHERE kind IN ('home', 'work');
CREATE UNIQUE INDEX IF NOT EXISTS idx_saved_places_label ON saved_places(label COLLATE NOCASE);
