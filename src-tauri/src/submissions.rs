//! User-submitted cameras: local storage, validation, proximity checks, OSM tagging
//! scheme and JOSM `.osm` export.

use crate::db::{self, Camera};
use crate::error::{AppError, AppResult};
use crate::geo_util::{haversine_m, valid_coord};
use crate::grid::circle_bbox;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const DUPLICATE_RADIUS_M: f64 = 15.0;
pub const CATEGORIES: [&str; 3] = ["flock", "alpr", "unsure"];
pub const MOUNTS: [&str; 4] = ["pole", "mast", "building", "other"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Submission {
    pub id: i64,
    pub lat: f64,
    pub lon: f64,
    pub category: String,
    pub direction: Option<i64>,
    pub mount: Option<String>,
    pub operator: Option<String>,
    /// Private. Never leaves the machine.
    pub notes: Option<String>,
    pub status: String,
    pub osm_element_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmissionInput {
    pub lat: f64,
    pub lon: f64,
    pub category: String,
    #[serde(default)]
    pub direction: Option<i64>,
    #[serde(default)]
    pub mount: Option<String>,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl SubmissionInput {
    pub fn validate(&self) -> AppResult<()> {
        if !valid_coord(self.lat, self.lon) {
            return Err(AppError::Invalid(
                "coordinates are outside the valid range (lat ±90, lon ±180)".into(),
            ));
        }
        if !CATEGORIES.contains(&self.category.as_str()) {
            return Err(AppError::Invalid(format!(
                "category must be one of {}",
                CATEGORIES.join(", ")
            )));
        }
        if let Some(d) = self.direction {
            if !(0..=359).contains(&d) {
                return Err(AppError::Invalid("direction must be 0–359 degrees".into()));
            }
        }
        if let Some(m) = clean(&self.mount) {
            if !MOUNTS.contains(&m.as_str()) {
                return Err(AppError::Invalid(format!(
                    "mount must be one of {}",
                    MOUNTS.join(", ")
                )));
            }
        }
        Ok(())
    }
}

const COLS: &str = "id, lat, lon, category, direction, mount, operator, notes, status, osm_element_id, created_at, updated_at";

fn row_to_sub(row: &rusqlite::Row) -> rusqlite::Result<Submission> {
    Ok(Submission {
        id: row.get(0)?,
        lat: row.get(1)?,
        lon: row.get(2)?,
        category: row.get(3)?,
        direction: row.get(4)?,
        mount: row.get(5)?,
        operator: row.get(6)?,
        notes: row.get(7)?,
        status: row.get(8)?,
        osm_element_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

pub fn list(conn: &Connection) -> AppResult<Vec<Submission>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM submissions ORDER BY created_at DESC"))?;
    let rows = stmt.query_map([], row_to_sub)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Option<Submission>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM submissions WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], row_to_sub).optional()?)
}

pub fn create(conn: &Connection, input: &SubmissionInput) -> AppResult<Submission> {
    input.validate()?;
    let now = db::now();
    conn.execute(
        "INSERT INTO submissions(lat, lon, category, direction, mount, operator, notes, status, osm_element_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'local', NULL, ?8, ?8)",
        params![
            input.lat,
            input.lon,
            input.category,
            input.direction,
            clean(&input.mount),
            clean(&input.operator),
            clean(&input.notes),
            now
        ],
    )?;
    let id = conn.last_insert_rowid();
    get(conn, id)?.ok_or_else(|| AppError::Other("submission vanished after insert".into()))
}

pub fn update(conn: &Connection, id: i64, input: &SubmissionInput) -> AppResult<Submission> {
    input.validate()?;
    let existing = get(conn, id)?.ok_or_else(|| AppError::Invalid(format!("submission {id} not found")))?;
    if existing.status != "local" {
        return Err(AppError::Invalid(
            "this submission has already been uploaded to OSM; edit it on OSM instead".into(),
        ));
    }
    conn.execute(
        "UPDATE submissions SET lat = ?2, lon = ?3, category = ?4, direction = ?5, mount = ?6,
         operator = ?7, notes = ?8, updated_at = ?9 WHERE id = ?1",
        params![
            id,
            input.lat,
            input.lon,
            input.category,
            input.direction,
            clean(&input.mount),
            clean(&input.operator),
            clean(&input.notes),
            db::now()
        ],
    )?;
    get(conn, id)?.ok_or_else(|| AppError::Other("submission vanished after update".into()))
}

pub fn delete(conn: &Connection, id: i64) -> AppResult<bool> {
    Ok(conn.execute("DELETE FROM submissions WHERE id = ?1", params![id])? > 0)
}

pub fn mark_uploaded(conn: &Connection, id: i64, osm_element_id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE submissions SET status = 'uploaded', osm_element_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, osm_element_id, db::now()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct NearbySubmission {
    pub submission: Submission,
    pub distance_m: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NearbyCamera {
    pub camera: Camera,
    pub distance_m: f64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Proximity {
    pub submissions: Vec<NearbySubmission>,
    pub cameras: Vec<NearbyCamera>,
}

/// Local submissions and cached OSM cameras within `DUPLICATE_RADIUS_M` of a point.
pub fn proximity(conn: &Connection, lat: f64, lon: f64, exclude_id: Option<i64>) -> AppResult<Proximity> {
    let bbox = circle_bbox(lat, lon, DUPLICATE_RADIUS_M * 2.0);
    let mut out = Proximity::default();
    for s in list(conn)? {
        if Some(s.id) == exclude_id || !bbox.contains(s.lat, s.lon) {
            continue;
        }
        let d = haversine_m(lat, lon, s.lat, s.lon);
        if d <= DUPLICATE_RADIUS_M {
            out.submissions.push(NearbySubmission { submission: s, distance_m: d });
        }
    }
    for c in db::cameras_in_bbox(conn, &bbox)? {
        let d = haversine_m(lat, lon, c.lat, c.lon);
        if d <= DUPLICATE_RADIUS_M {
            out.cameras.push(NearbyCamera { camera: c, distance_m: d });
        }
    }
    out.submissions.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    out.cameras.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    Ok(out)
}

/// The OSM tags a submission would be written with. Notes are deliberately absent.
pub fn osm_tags(sub: &Submission) -> Vec<(String, String)> {
    let mut tags: Vec<(String, String)> = vec![
        ("man_made".into(), "surveillance".into()),
        ("surveillance".into(), "public".into()),
        ("surveillance:type".into(), "ALPR".into()),
        ("surveillance:zone".into(), "traffic".into()),
        ("camera:type".into(), "fixed".into()),
    ];
    if sub.category == "flock" {
        tags.push(("brand".into(), "Flock Safety".into()));
        tags.push(("manufacturer".into(), "Flock Safety".into()));
    }
    match sub.mount.as_deref() {
        Some("pole") => tags.push(("camera:mount".into(), "pole".into())),
        Some("mast") => tags.push(("camera:mount".into(), "mast".into())),
        Some("building") => tags.push(("camera:mount".into(), "wall".into())),
        _ => {}
    }
    if let Some(d) = sub.direction {
        tags.push(("direction".into(), d.to_string()));
    }
    if let Some(op) = clean(&sub.operator) {
        tags.push(("operator".into(), op));
    }
    tags
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// JOSM-compatible `.osm` document with negative (new) ids, ready for manual review
/// and upload from JOSM. Private notes are never included.
pub fn josm_xml(subs: &[Submission]) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version='1.0' encoding='UTF-8'?>\n");
    xml.push_str(&format!(
        "<osm version='0.6' generator='FlockFinder/{}' upload='true'>\n",
        env!("CARGO_PKG_VERSION")
    ));
    for (i, sub) in subs.iter().enumerate() {
        let id = -(i as i64 + 1);
        xml.push_str(&format!(
            "  <node id='{id}' action='modify' visible='true' lat='{:.7}' lon='{:.7}'>\n",
            sub.lat, sub.lon
        ));
        for (k, v) in osm_tags(sub) {
            xml.push_str(&format!(
                "    <tag k='{}' v='{}'/>\n",
                xml_escape(&k),
                xml_escape(&v)
            ));
        }
        xml.push_str("  </node>\n");
    }
    xml.push_str("</osm>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn input(lat: f64, lon: f64) -> SubmissionInput {
        SubmissionInput {
            lat,
            lon,
            category: "flock".into(),
            direction: Some(90),
            mount: Some("pole".into()),
            operator: Some("  City PD ".into()),
            notes: Some("SECRET private note".into()),
        }
    }

    #[test]
    fn validation_rejects_bad_input() {
        assert!(SubmissionInput { lat: 91.0, ..input(0.0, 0.0) }.validate().is_err());
        assert!(SubmissionInput { category: "drone".into(), ..input(0.0, 0.0) }.validate().is_err());
        assert!(SubmissionInput { direction: Some(360), ..input(0.0, 0.0) }.validate().is_err());
        assert!(SubmissionInput { mount: Some("tree".into()), ..input(0.0, 0.0) }.validate().is_err());
        assert!(input(39.7, -105.0).validate().is_ok());
    }

    #[test]
    fn crud_round_trip() {
        let c = test_conn();
        let s = create(&c, &input(39.7, -105.0)).unwrap();
        assert_eq!(s.status, "local");
        assert_eq!(s.operator.as_deref(), Some("City PD"));
        assert_eq!(list(&c).unwrap().len(), 1);
        let u = update(&c, s.id, &SubmissionInput { category: "alpr".into(), ..input(39.71, -105.0) }).unwrap();
        assert_eq!(u.category, "alpr");
        mark_uploaded(&c, s.id, 4242).unwrap();
        let g = get(&c, s.id).unwrap().unwrap();
        assert_eq!(g.status, "uploaded");
        assert_eq!(g.osm_element_id, Some(4242));
        assert!(update(&c, s.id, &input(39.7, -105.0)).is_err());
        assert!(delete(&c, s.id).unwrap());
        assert!(!delete(&c, s.id).unwrap());
    }

    #[test]
    fn proximity_flags_nearby_only() {
        let c = test_conn();
        let a = create(&c, &input(39.7000, -105.0000)).unwrap();
        create(&c, &input(39.7000, -105.0001)).unwrap(); // ≈ 8.6 m away
        create(&c, &input(39.7010, -105.0000)).unwrap(); // ≈ 111 m away
        let p = proximity(&c, 39.7000, -105.0000, Some(a.id)).unwrap();
        assert_eq!(p.submissions.len(), 1);
        assert!(p.submissions[0].distance_m < DUPLICATE_RADIUS_M);
        assert!(p.cameras.is_empty());
    }

    #[test]
    fn josm_export_uses_tag_scheme_and_omits_notes() {
        let c = test_conn();
        let s = create(&c, &input(39.7, -105.0)).unwrap();
        let unsure = create(
            &c,
            &SubmissionInput {
                category: "unsure".into(),
                mount: Some("building".into()),
                operator: Some("A & B <Co>".into()),
                ..input(39.71, -105.01)
            },
        )
        .unwrap();
        let xml = josm_xml(&[s, unsure]);
        assert!(xml.contains("<node id='-1'"));
        assert!(xml.contains("<node id='-2'"));
        assert!(xml.contains("k='surveillance:type' v='ALPR'"));
        assert!(xml.contains("k='brand' v='Flock Safety'"));
        assert!(xml.contains("k='direction' v='90'"));
        assert!(xml.contains("k='camera:mount' v='wall'"));
        assert!(xml.contains("v='A &amp; B &lt;Co&gt;'"));
        assert!(!xml.contains("SECRET"));
        assert!(!xml.contains("note"));
        assert_eq!(xml.matches("k='brand'").count(), 1);
    }
}
