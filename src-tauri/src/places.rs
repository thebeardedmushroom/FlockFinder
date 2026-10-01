//! Saved places: Home, Work and up to 10 custom places, kept on this device only.
//!
//! Each place keeps the coordinates of the geocoding result (or map pin) it was saved from, so
//! starting a trip to it never geocodes the address again.

use crate::db;
use crate::error::{AppError, AppResult};
use crate::geo_util::valid_coord;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Custom places allowed besides Home and Work.
pub const MAX_CUSTOM: usize = 10;
pub const MAX_LABEL_CHARS: usize = 60;
pub const MAX_ADDRESS_CHARS: usize = 400;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SavedPlace {
    pub id: i64,
    /// `home`, `work` or `custom`.
    pub kind: String,
    pub label: String,
    pub address: String,
    pub lat: f64,
    pub lon: f64,
    pub created_at: i64,
    pub sort_order: i64,
}

/// A place to save. For Home and Work the slot is replaced (the label is fixed); a custom place
/// is updated when `id` is set and added otherwise.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaceInput {
    #[serde(default)]
    pub id: Option<i64>,
    pub kind: String,
    #[serde(default)]
    pub label: String,
    pub address: String,
    pub lat: f64,
    pub lon: f64,
}

fn slot_label(kind: &str) -> Option<&'static str> {
    match kind {
        "home" => Some("Home"),
        "work" => Some("Work"),
        _ => None,
    }
}

const COLS: &str = "id, kind, label, address, lat, lon, created_at, sort_order";

fn row_to_place(row: &rusqlite::Row) -> rusqlite::Result<SavedPlace> {
    Ok(SavedPlace {
        id: row.get(0)?,
        kind: row.get(1)?,
        label: row.get(2)?,
        address: row.get(3)?,
        lat: row.get(4)?,
        lon: row.get(5)?,
        created_at: row.get(6)?,
        sort_order: row.get(7)?,
    })
}

/// Home, Work, then custom places in their saved order.
pub fn list(conn: &Connection) -> AppResult<Vec<SavedPlace>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM saved_places
         ORDER BY CASE kind WHEN 'home' THEN 0 WHEN 'work' THEN 1 ELSE 2 END, sort_order, id"
    ))?;
    let rows = stmt.query_map([], row_to_place)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Option<SavedPlace>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM saved_places WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], row_to_place).optional()?)
}

fn slot(conn: &Connection, kind: &str) -> AppResult<Option<SavedPlace>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLS} FROM saved_places WHERE kind = ?1"))?;
    Ok(stmt.query_row(params![kind], row_to_place).optional()?)
}

/// Another place already uses this label (case-insensitive). Home and Work are reserved even
/// while unset, so a custom place can't be confused with them.
fn label_taken(conn: &Connection, label: &str, except_id: Option<i64>) -> AppResult<bool> {
    let wanted = label.to_lowercase();
    if wanted == "home" || wanted == "work" {
        return Ok(true);
    }
    Ok(list(conn)?
        .iter()
        .any(|p| Some(p.id) != except_id && p.label.to_lowercase() == wanted))
}

pub fn save(conn: &Connection, input: &PlaceInput) -> AppResult<SavedPlace> {
    if !valid_coord(input.lat, input.lon) {
        return Err(AppError::Invalid(
            "coordinates are outside the valid range (lat ±90, lon ±180)".into(),
        ));
    }
    let address = input.address.trim();
    if address.is_empty() {
        return Err(AppError::Invalid("choose an address for the place".into()));
    }
    let address: String = address.chars().take(MAX_ADDRESS_CHARS).collect();
    let now = db::now();

    if let Some(label) = slot_label(&input.kind) {
        // Home and Work: one each, replaced in place.
        if let Some(existing) = slot(conn, &input.kind)? {
            conn.execute(
                "UPDATE saved_places SET address = ?2, lat = ?3, lon = ?4, created_at = ?5 WHERE id = ?1",
                params![existing.id, address, input.lat, input.lon, now],
            )?;
            return get(conn, existing.id)?.ok_or_else(|| AppError::Other("saved place vanished after update".into()));
        }
        conn.execute(
            "INSERT INTO saved_places(kind, label, address, lat, lon, created_at, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![input.kind, label, address, input.lat, input.lon, now],
        )?;
        let id = conn.last_insert_rowid();
        return get(conn, id)?.ok_or_else(|| AppError::Other("saved place vanished after insert".into()));
    }
    if input.kind != "custom" {
        return Err(AppError::Invalid("a place is Home, Work or a custom place".into()));
    }

    let label = input.label.trim();
    if label.is_empty() {
        return Err(AppError::Invalid("enter a name for the place".into()));
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(AppError::Invalid(format!("keep the name to {MAX_LABEL_CHARS} characters or fewer")));
    }
    if label_taken(conn, label, input.id)? {
        return Err(AppError::Invalid(format!("you already have a place named \"{label}\"")));
    }
    if let Some(id) = input.id {
        let existing = get(conn, id)?.ok_or_else(|| AppError::Invalid(format!("saved place {id} not found")))?;
        if existing.kind != "custom" {
            return Err(AppError::Invalid("Home and Work can't be renamed".into()));
        }
        conn.execute(
            "UPDATE saved_places SET label = ?2, address = ?3, lat = ?4, lon = ?5 WHERE id = ?1",
            params![id, label, address, input.lat, input.lon],
        )?;
        return get(conn, id)?.ok_or_else(|| AppError::Other("saved place vanished after update".into()));
    }
    let customs: i64 = conn.query_row("SELECT COUNT(*) FROM saved_places WHERE kind = 'custom'", [], |r| r.get(0))?;
    if customs as usize >= MAX_CUSTOM {
        return Err(AppError::Invalid(format!(
            "you can save up to {MAX_CUSTOM} places besides Home and Work; remove one first"
        )));
    }
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM saved_places WHERE kind = 'custom'",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO saved_places(kind, label, address, lat, lon, created_at, sort_order)
         VALUES ('custom', ?1, ?2, ?3, ?4, ?5, ?6)",
        params![label, address, input.lat, input.lon, now, next],
    )?;
    let id = conn.last_insert_rowid();
    get(conn, id)?.ok_or_else(|| AppError::Other("saved place vanished after insert".into()))
}

/// Delete a custom place, or clear Home or Work.
pub fn delete(conn: &Connection, id: i64) -> AppResult<bool> {
    Ok(conn.execute("DELETE FROM saved_places WHERE id = ?1", params![id])? > 0)
}

/// Put the custom places in this order. `ids` must list every custom place exactly once.
pub fn reorder(conn: &mut Connection, ids: &[i64]) -> AppResult<Vec<SavedPlace>> {
    let tx = conn.transaction()?;
    {
        let mut current: Vec<i64> = list(&tx)?.into_iter().filter(|p| p.kind == "custom").map(|p| p.id).collect();
        let mut wanted = ids.to_vec();
        current.sort_unstable();
        wanted.sort_unstable();
        if current != wanted {
            return Err(AppError::Invalid("the saved places changed; reopen the list and try again".into()));
        }
        let mut stmt = tx.prepare_cached("UPDATE saved_places SET sort_order = ?2 WHERE id = ?1")?;
        for (i, id) in ids.iter().enumerate() {
            stmt.execute(params![id, i as i64])?;
        }
    }
    tx.commit()?;
    list(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(label: &str) -> PlaceInput {
        PlaceInput { id: None, kind: "custom".into(), label: label.into(), address: format!("{label} St"), lat: 39.7, lon: -104.9 }
    }

    fn slot_input(kind: &str, address: &str) -> PlaceInput {
        PlaceInput { id: None, kind: kind.into(), label: String::new(), address: address.into(), lat: 33.78, lon: -84.38 }
    }

    #[test]
    fn home_and_work_are_single_slots_with_fixed_labels() {
        let conn = db::test_conn();
        let home = save(&conn, &PlaceInput { label: "My house".into(), ..slot_input("home", "1 Main St") }).unwrap();
        assert_eq!(home.label, "Home");
        let again = save(&conn, &slot_input("home", "2 Oak Ave")).unwrap();
        assert_eq!(again.id, home.id, "setting Home again replaces it");
        assert_eq!(again.address, "2 Oak Ave");
        save(&conn, &slot_input("work", "3 Office Pk")).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["Home", "Work"]);
    }

    #[test]
    fn custom_labels_are_unique_ignoring_case_and_home_work_are_reserved() {
        let conn = db::test_conn();
        save(&conn, &custom("Gym")).unwrap();
        assert!(matches!(save(&conn, &custom("gYM")), Err(AppError::Invalid(_))));
        assert!(matches!(save(&conn, &custom("home")), Err(AppError::Invalid(_))));
        assert!(matches!(save(&conn, &custom("  ")), Err(AppError::Invalid(_))));
        // Renaming a place to its own name (different case) is fine.
        let gym = list(&conn).unwrap()[0].clone();
        let renamed = save(&conn, &PlaceInput { id: Some(gym.id), ..custom("GYM") }).unwrap();
        assert_eq!(renamed.label, "GYM");
    }

    #[test]
    fn at_most_ten_custom_places() {
        let conn = db::test_conn();
        for i in 0..MAX_CUSTOM {
            save(&conn, &custom(&format!("Place {i}"))).unwrap();
        }
        save(&conn, &slot_input("home", "1 Main St")).unwrap();
        let err = save(&conn, &custom("One too many")).unwrap_err();
        assert!(err.to_string().contains("up to 10"), "{err}");
    }

    #[test]
    fn rejects_bad_coordinates_and_missing_addresses() {
        let conn = db::test_conn();
        assert!(save(&conn, &PlaceInput { lat: 91.0, ..custom("A") }).is_err());
        assert!(save(&conn, &PlaceInput { lon: f64::NAN, ..custom("A") }).is_err());
        assert!(save(&conn, &PlaceInput { address: " ".into(), ..custom("A") }).is_err());
        assert!(save(&conn, &PlaceInput { kind: "school".into(), ..custom("A") }).is_err());
    }

    #[test]
    fn reorder_sets_the_custom_order_and_delete_clears_a_slot() {
        let mut conn = db::test_conn();
        let a = save(&conn, &custom("A")).unwrap();
        let b = save(&conn, &custom("B")).unwrap();
        let c = save(&conn, &custom("C")).unwrap();
        let home = save(&conn, &slot_input("home", "1 Main St")).unwrap();
        let after = reorder(&mut conn, &[c.id, a.id, b.id]).unwrap();
        assert_eq!(after.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["Home", "C", "A", "B"]);
        assert!(reorder(&mut conn, &[c.id, a.id]).is_err(), "a partial order is refused");
        // A new place goes last.
        save(&conn, &custom("D")).unwrap();
        assert_eq!(list(&conn).unwrap().last().unwrap().label, "D");
        assert!(delete(&conn, home.id).unwrap());
        assert!(list(&conn).unwrap().iter().all(|p| p.kind == "custom"));
    }
}
