//! Cell-oriented fetching: decide which cells need Overpass, query them politely
//! (sequentially, bounded bbox per request), and commit results atomically.

use crate::db;
use crate::error::{AppError, AppResult};
use crate::grid::{union_bbox, Cell};
use crate::overpass;
use crate::state::AppState;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    /// One request covering the union bbox of every stale cell. Used for viewport
    /// fetches, where the caller has already split the viewport at the antimeridian
    /// and the union is bounded by the viewport itself.
    SingleBbox,
    /// One request per 1°×1° super-cell that contains a stale cell. Used for alert
    /// refreshes, whose targets may be scattered across the globe.
    SuperCells,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct FetchStats {
    pub requests: usize,
    pub cells_fetched: usize,
    pub elements: usize,
    pub skipped: usize,
}

/// Fetch every cell in `cells` that is uncached or older than the TTL (or all of them
/// when `force`). Returns without any network activity when nothing is stale.
pub async fn fetch_cells(
    state: &AppState,
    cells: &[Cell],
    force: bool,
    grouping: Grouping,
) -> AppResult<FetchStats> {
    let mut stats = FetchStats::default();
    if cells.is_empty() {
        return Ok(stats);
    }

    let (settings, stale) = {
        let conn = state.conn();
        let settings = db::load_settings(&conn)?;
        let stale = if force {
            cells.to_vec()
        } else {
            db::stale_cells(&conn, cells, settings.ttl_secs(), db::now())?
        };
        (settings, stale)
    };
    if stale.is_empty() {
        return Ok(stats);
    }

    let groups: Vec<Vec<Cell>> = match grouping {
        Grouping::SingleBbox => vec![stale],
        Grouping::SuperCells => {
            let mut map: BTreeMap<(i32, i32), Vec<Cell>> = BTreeMap::new();
            for c in stale {
                map.entry((c.row.div_euclid(20), c.col.div_euclid(20)))
                    .or_default()
                    .push(c);
            }
            map.into_values().collect()
        }
    };

    for group in groups {
        let Some(area) = union_bbox(&group) else { continue };
        if area.crosses_antimeridian() {
            return Err(AppError::Invalid(
                "internal: fetch group crosses the antimeridian; split first".into(),
            ));
        }
        // Everything inside the union bbox is fetched, so stamp every cell in it —
        // including non-stale gaps — as fresh.
        let covered = crate::grid::cells_for_bbox(&area);
        let parsed = overpass::fetch(&state.http, &settings.overpass_endpoint, &area).await?;
        stats.requests += 1;
        stats.elements += parsed.elements.len();
        stats.skipped += parsed.skipped;
        let now = db::now();
        let mut conn = state.conn();
        let ingest = db::ingest_fetch(&mut conn, &area, &covered, &parsed.elements, now)?;
        stats.cells_fetched += ingest.cells_marked;
        db::purge_stale(&conn, now)?;
        log::info!(
            "fetched {} ({} elements, {} skipped) → {} upserted, {} marked stale, {} cells",
            area.overpass(),
            parsed.elements.len(),
            parsed.skipped,
            ingest.upserted,
            ingest.marked_stale,
            ingest.cells_marked
        );
    }
    Ok(stats)
}
