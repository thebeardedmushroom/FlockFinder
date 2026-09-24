-- Migration 0006: a change counter for the cameras table. The map's camera snapshot is
-- cached on disk under this number and only rebuilt after a camera was added, changed or
-- removed, so a warm start doesn't re-encode the whole table.

CREATE TABLE IF NOT EXISTS camera_version (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  v  INTEGER NOT NULL
);
INSERT OR IGNORE INTO camera_version(id, v) VALUES (1, 0);

CREATE TRIGGER IF NOT EXISTS cameras_version_ins AFTER INSERT ON cameras
BEGIN UPDATE camera_version SET v = v + 1 WHERE id = 1; END;
CREATE TRIGGER IF NOT EXISTS cameras_version_upd AFTER UPDATE ON cameras
BEGIN UPDATE camera_version SET v = v + 1 WHERE id = 1; END;
CREATE TRIGGER IF NOT EXISTS cameras_version_del AFTER DELETE ON cameras
BEGIN UPDATE camera_version SET v = v + 1 WHERE id = 1; END;
