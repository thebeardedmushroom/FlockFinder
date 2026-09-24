//! Compact binary snapshot of every camera, for the map's clustering worker.
//!
//! The map aggregates the whole dataset at every zoom level, so it needs every camera's
//! position, category and filterable fields, but not the full tag set. Sending that as JSON
//! objects would cost far more than the data itself; this layout lets the frontend view the
//! numeric sections as typed arrays directly.
//!
//! Layout (little-endian):
//!
//! | offset          | content                                                          |
//! |-----------------|------------------------------------------------------------------|
//! | 0               | magic `FFP1`                                                     |
//! | 4               | camera count `n` (u32)                                           |
//! | 8               | meta JSON length (u32)                                           |
//! | 12              | reserved (u32, 0)                                                |
//! | 16              | longitude, f64 × n                                               |
//! | 16 + 8n         | latitude, f64 × n                                                |
//! | 16 + 16n        | OSM id, f64 × n (exact up to 2^53)                               |
//! | 16 + 24n        | operator index, u32 × n (0 = none, i ≥ 1 → `operators[i - 1]`)   |
//! | 16 + 28n        | flags, u8 × n (see `FLAG_*`)                                     |
//! | aligned to 4    | meta JSON: `{"operators": [...], "directions": [[i, "raw"], ...]}` |

use crate::error::AppResult;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const MAGIC: &[u8; 4] = b"FFP1";
const HEADER_LEN: usize = 16;

/// Bits 0–1: marker kind (0 Flock, 1 other ALPR, which includes `unknown`).
pub const FLAG_KIND_MASK: u8 = 0b11;
pub const KIND_FLOCK: u8 = 0;
pub const KIND_ALPR: u8 = 1;
/// Bit 2: absent from the latest data (kept hollow for 30 days, not counted).
pub const FLAG_STALE: u8 = 1 << 2;
/// Bits 4–5: OSM element type (0 node, 1 way, 2 relation).
pub const FLAG_TYPE_SHIFT: u8 = 4;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub operators: Vec<String>,
    /// `[index, raw direction tag]` for cameras with a `direction` or `camera:direction`.
    pub directions: Vec<(u32, String)>,
}

fn type_code(osm_type: &str) -> u8 {
    match osm_type {
        "way" => 1,
        "relation" => 2,
        _ => 0,
    }
}

/// The cameras table's change counter (migration 0006 triggers bump it on every write).
pub fn camera_version(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT v FROM camera_version WHERE id = 1", [], |r| r.get(0))?)
}

/// The snapshot, from the on-disk cache when it was built at the current camera version,
/// otherwise freshly encoded and written back. The cache file is the snapshot prefixed with
/// the version it belongs to (i64 LE). Returns the bytes and whether they came from the cache.
pub fn cached_or_encode(conn: &Connection, path: &Path) -> AppResult<(Vec<u8>, bool)> {
    let version = camera_version(conn)?;
    if let Ok(mut buf) = std::fs::read(path) {
        if buf.len() >= 8 && i64::from_le_bytes(buf[..8].try_into().unwrap_or_default()) == version {
            buf.drain(..8);
            return Ok((buf, true));
        }
    }
    let body = encode(conn)?;
    let mut file = Vec::with_capacity(8 + body.len());
    file.extend_from_slice(&version.to_le_bytes());
    file.extend_from_slice(&body);
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &file).and_then(|_| std::fs::rename(&tmp, path)) {
        log::warn!("could not cache the camera snapshot at {}: {e}", path.display());
    }
    Ok((body, false))
}

/// Encode every row of the cameras table.
pub fn encode(conn: &Connection) -> AppResult<Vec<u8>> {
    let mut stmt = conn.prepare_cached(
        "SELECT osm_type, osm_id, lat, lon, category, stale_since IS NOT NULL,
                json_extract(tags_json, '$.operator'),
                coalesce(json_extract(tags_json, '$.direction'), json_extract(tags_json, '$.\"camera:direction\"'))
         FROM cameras",
    )?;
    let mut lon = Vec::new();
    let mut lat = Vec::new();
    let mut ids = Vec::new();
    let mut ops: Vec<u32> = Vec::new();
    let mut flags = Vec::new();
    let mut meta = Meta::default();
    let mut op_index: HashMap<String, u32> = HashMap::new();

    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let osm_type: String = row.get(0)?;
        let osm_id: i64 = row.get(1)?;
        let category: String = row.get(4)?;
        let stale: bool = row.get(5)?;
        let operator: Option<String> = row.get(6)?;
        let direction: Option<String> = row.get(7)?;
        let i = lon.len() as u32;

        lat.push(row.get::<_, f64>(2)?);
        lon.push(row.get::<_, f64>(3)?);
        ids.push(osm_id as f64);
        let kind = if category == "flock" { KIND_FLOCK } else { KIND_ALPR };
        flags.push(kind | if stale { FLAG_STALE } else { 0 } | (type_code(&osm_type) << FLAG_TYPE_SHIFT));
        ops.push(match operator.filter(|o| !o.trim().is_empty()) {
            None => 0,
            Some(o) => {
                let next = meta.operators.len() as u32 + 1;
                *op_index.entry(o.clone()).or_insert_with(|| {
                    meta.operators.push(o);
                    next
                })
            }
        });
        if let Some(d) = direction.filter(|d| !d.trim().is_empty()) {
            meta.directions.push((i, d));
        }
    }

    let n = lon.len();
    let meta_json = serde_json::to_vec(&meta)?;
    let body_len = HEADER_LEN + n * 29;
    let pad = (4 - body_len % 4) % 4;
    let mut out = Vec::with_capacity(body_len + pad + meta_json.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.extend_from_slice(&(meta_json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for v in lon.iter().chain(&lat).chain(&ids) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &ops {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&flags);
    out.extend(std::iter::repeat(0u8).take(pad));
    out.extend_from_slice(&meta_json);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ingest_global, test_conn};
    use crate::overpass::ParsedElement;
    use std::collections::BTreeMap;

    fn el(osm_type: &str, id: i64, lat: f64, lon: f64, tags: &[(&str, &str)]) -> ParsedElement {
        ParsedElement {
            osm_type: osm_type.into(),
            osm_id: id,
            lat,
            lon,
            tags: tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        }
    }

    fn f64_at(b: &[u8], off: usize) -> f64 {
        f64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }
    fn u32_at(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn round_trips_positions_flags_operators_and_directions() {
        let mut c = test_conn();
        let elements = vec![
            el("node", 9_007_199_254, 39.5, -104.9, &[("brand", "Flock Safety"), ("operator", "Denver PD"), ("direction", "90;270")]),
            el("way", 42, -16.5, 179.99, &[("surveillance:type", "ALPR"), ("camera:direction", "NE")]),
            el("node", 7, 51.0, 0.1, &[("surveillance:type", "ALPR"), ("operator", "Denver PD")]),
        ];
        ingest_global(&mut c, &elements, 100, 100).unwrap();
        // A fourth camera that a later sync no longer returns becomes stale.
        ingest_global(&mut c, &elements[..2], 200, 200).unwrap();

        let b = encode(&c).unwrap();
        assert_eq!(&b[0..4], MAGIC);
        let n = u32_at(&b, 4) as usize;
        assert_eq!(n, 3);
        let meta_len = u32_at(&b, 8) as usize;
        let body = HEADER_LEN + n * 29;
        let meta_off = body + (4 - body % 4) % 4;
        assert_eq!(meta_off % 4, 0);
        assert_eq!(b.len(), meta_off + meta_len);
        let meta: Meta = serde_json::from_slice(&b[meta_off..]).unwrap();
        assert_eq!(meta.operators, vec!["Denver PD".to_string()]);

        let mut seen = 0;
        for i in 0..n {
            let lon = f64_at(&b, HEADER_LEN + 8 * i);
            let lat = f64_at(&b, HEADER_LEN + 8 * n + 8 * i);
            let id = f64_at(&b, HEADER_LEN + 16 * n + 8 * i) as i64;
            let op = u32_at(&b, HEADER_LEN + 24 * n + 4 * i);
            let flags = b[HEADER_LEN + 28 * n + i];
            let dir = meta.directions.iter().find(|(j, _)| *j as usize == i).map(|(_, d)| d.as_str());
            match id {
                9_007_199_254 => {
                    assert_eq!((lat, lon), (39.5, -104.9));
                    assert_eq!(flags & FLAG_KIND_MASK, KIND_FLOCK);
                    assert_eq!(flags & FLAG_STALE, 0);
                    assert_eq!(op, 1);
                    assert_eq!(dir, Some("90;270"));
                }
                42 => {
                    assert_eq!(flags >> FLAG_TYPE_SHIFT, 1, "way");
                    assert_eq!(flags & FLAG_KIND_MASK, KIND_ALPR);
                    assert_eq!(op, 0);
                    assert_eq!(dir, Some("NE"), "falls back to camera:direction");
                }
                7 => {
                    assert_ne!(flags & FLAG_STALE, 0);
                    assert_eq!(op, 1, "operator strings are shared");
                    assert_eq!(dir, None);
                }
                other => panic!("unexpected id {other}"),
            }
            seen += 1;
        }
        assert_eq!(seen, 3);
    }

    #[test]
    fn snapshot_cache_is_reused_until_a_camera_changes() {
        let mut c = test_conn();
        let dir = std::env::temp_dir().join(format!("ff-points-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("camera-points.bin");
        let _ = std::fs::remove_file(&path);

        let v0 = camera_version(&c).unwrap();
        ingest_global(&mut c, &[el("node", 1, 1.0, 2.0, &[("surveillance:type", "ALPR")])], 10, 10).unwrap();
        assert!(camera_version(&c).unwrap() > v0, "insert bumps the version");

        let (first, cached) = cached_or_encode(&c, &path).unwrap();
        assert!(!cached);
        let (again, cached) = cached_or_encode(&c, &path).unwrap();
        assert!(cached, "unchanged table → served from the cache file");
        assert_eq!(first, again);

        // A re-sync (update), a new camera (insert) and a purge (delete) each invalidate it.
        ingest_global(&mut c, &[el("node", 1, 1.0, 2.5, &[("surveillance:type", "ALPR")])], 20, 20).unwrap();
        let (moved, cached) = cached_or_encode(&c, &path).unwrap();
        assert!(!cached);
        assert_ne!(moved, first);
        c.execute("DELETE FROM cameras", []).unwrap();
        assert!(!cached_or_encode(&c, &path).unwrap().1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_table_encodes_to_a_valid_header() {
        let c = test_conn();
        let b = encode(&c).unwrap();
        assert_eq!(u32_at(&b, 4), 0);
        let meta: Meta = serde_json::from_slice(&b[HEADER_LEN..]).unwrap();
        assert!(meta.operators.is_empty() && meta.directions.is_empty());
    }
}
