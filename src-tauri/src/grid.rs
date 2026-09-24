//! Bounding boxes and the fixed 0.05° cache grid.
//!
//! A cell is addressed by integer `(row, col)` where `row = floor(lat / 0.05)`
//! and `col = floor(lon / 0.05)`. Its key is `"{row}:{col}"`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CELL_SIZE: f64 = 0.05;
/// Rounding tolerance in units of cells (≈ 5.5 µm on the ground).
const EPS: f64 = 1e-9;

/// Geographic bounding box. `west > east` means the box crosses the antimeridian.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl BBox {
    pub fn new(south: f64, west: f64, north: f64, east: f64) -> Self {
        BBox {
            south,
            west,
            north,
            east,
        }
    }

    /// Clamp latitudes to ±90 and wrap longitudes to [-180, 180].
    pub fn sanitized(&self) -> BBox {
        let (mut south, mut north) = (
            self.south.clamp(-90.0, 90.0),
            self.north.clamp(-90.0, 90.0),
        );
        if south > north {
            std::mem::swap(&mut south, &mut north);
        }
        let width = self.east - self.west;
        if width >= 360.0 {
            return BBox::new(south, -180.0, north, 180.0);
        }
        // An east edge of exactly 180 stays 180 (wrapping it to -180 would turn an
        // ordinary box into an antimeridian-crossing one).
        let east = if (self.east - 180.0).abs() < 1e-12 {
            180.0
        } else {
            wrap_lon(self.east)
        };
        BBox::new(south, wrap_lon(self.west), north, east)
    }

    pub fn crosses_antimeridian(&self) -> bool {
        self.west > self.east
    }

    /// Split an antimeridian-crossing box into two ordinary boxes.
    pub fn split_antimeridian(&self) -> Vec<BBox> {
        if self.crosses_antimeridian() {
            vec![
                BBox::new(self.south, self.west, self.north, 180.0),
                BBox::new(self.south, -180.0, self.north, self.east),
            ]
        } else {
            vec![*self]
        }
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        if lat < self.south || lat > self.north {
            return false;
        }
        if self.crosses_antimeridian() {
            lon >= self.west || lon <= self.east
        } else {
            lon >= self.west && lon <= self.east
        }
    }

    /// Overpass bbox syntax: `south,west,north,east`.
    pub fn overpass(&self) -> String {
        format!(
            "{:.6},{:.6},{:.6},{:.6}",
            self.south, self.west, self.north, self.east
        )
    }
}

pub fn wrap_lon(lon: f64) -> f64 {
    if lon.is_nan() {
        return 0.0;
    }
    let mut l = (lon + 180.0) % 360.0;
    if l < 0.0 {
        l += 360.0;
    }
    l - 180.0
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Cell {
    pub row: i32,
    pub col: i32,
}

impl Cell {
    pub fn containing(lat: f64, lon: f64) -> Cell {
        let lat = lat.clamp(-90.0, 90.0);
        let lon = wrap_lon(lon);
        // The tiny nudge keeps values that are mathematically on a boundary (39.75 / 0.05)
        // from landing one cell early because of floating-point rounding.
        let row = ((lat / CELL_SIZE + EPS).floor() as i32).clamp(-1800, 1799);
        let col = ((lon / CELL_SIZE + EPS).floor() as i32).clamp(-3600, 3599);
        Cell { row, col }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.row, self.col)
    }

    pub fn bbox(&self) -> BBox {
        let south = self.row as f64 * CELL_SIZE;
        let west = self.col as f64 * CELL_SIZE;
        BBox::new(
            south.max(-90.0),
            west.max(-180.0),
            (south + CELL_SIZE).min(90.0),
            (west + CELL_SIZE).min(180.0),
        )
    }
}

/// All cells intersecting a bbox. Antimeridian-crossing boxes are split first.
pub fn cells_for_bbox(bbox: &BBox) -> Vec<Cell> {
    let bbox = bbox.sanitized();
    let mut out = BTreeSet::new();
    for part in bbox.split_antimeridian() {
        let sw = Cell::containing(part.south, part.west);
        // North/east edges: an edge exactly on a grid line does not include the next
        // cell, and lon 180 maps to the last column instead of wrapping to -180.
        let ne_row = (((part.north / CELL_SIZE) - EPS).ceil() as i32 - 1)
            .clamp(-1800, 1799)
            .max(sw.row);
        let ne_col = (((part.east / CELL_SIZE) - EPS).ceil() as i32 - 1)
            .clamp(-3600, 3599)
            .max(sw.col);
        for row in sw.row..=ne_row {
            for col in sw.col..=ne_col {
                out.insert(Cell { row, col });
            }
        }
    }
    out.into_iter().collect()
}

/// Smallest bbox covering a set of cells that all lie on the same side of the
/// antimeridian (i.e. a single `split_antimeridian` part). Returns `None` for empty input.
pub fn union_bbox(cells: &[Cell]) -> Option<BBox> {
    let first = cells.first()?;
    let mut b = first.bbox();
    for c in &cells[1..] {
        let cb = c.bbox();
        b.south = b.south.min(cb.south);
        b.west = b.west.min(cb.west);
        b.north = b.north.max(cb.north);
        b.east = b.east.max(cb.east);
    }
    Some(b)
}

/// Approximate bbox of a circle, converting metres to degrees at that latitude.
pub fn circle_bbox(lat: f64, lon: f64, radius_m: f64) -> BBox {
    let dlat = radius_m / 111_320.0;
    let cos = lat.to_radians().cos().abs().max(0.01);
    let dlon = radius_m / (111_320.0 * cos);
    BBox::new(
        (lat - dlat).max(-90.0),
        wrap_lon(lon - dlon),
        (lat + dlat).min(90.0),
        wrap_lon(lon + dlon),
    )
}

pub fn cells_for_circle(lat: f64, lon: f64, radius_m: f64) -> Vec<Cell> {
    cells_for_bbox(&circle_bbox(lat, lon, radius_m))
}

/// Cells intersected by a polyline buffered by `corridor_m`, computed per segment
/// so a long diagonal track does not pull in its whole bounding rectangle.
pub fn cells_for_polyline(points: &[(f64, f64)], corridor_m: f64) -> Vec<Cell> {
    let mut out = BTreeSet::new();
    if points.len() == 1 {
        out.extend(cells_for_circle(points[0].0, points[0].1, corridor_m));
    }
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let mid_lat = (a.0 + b.0) / 2.0;
        let dlat = corridor_m / 111_320.0;
        let cos = mid_lat.to_radians().cos().abs().max(0.01);
        let dlon = corridor_m / (111_320.0 * cos);
        let bbox = if (a.1 - b.1).abs() > 180.0 {
            // A single segment spanning > 180° of longitude crosses the antimeridian.
            BBox::new(
                a.0.min(b.0) - dlat,
                a.1.max(b.1) - dlon,
                a.0.max(b.0) + dlat,
                a.1.min(b.1) + dlon,
            )
        } else {
            BBox::new(
                a.0.min(b.0) - dlat,
                a.1.min(b.1) - dlon,
                a.0.max(b.0) + dlat,
                a.1.max(b.1) + dlon,
            )
        };
        out.extend(cells_for_bbox(&bbox));
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_containing_is_floor_based() {
        let c = Cell::containing(39.7392, -104.9903);
        assert_eq!(c.row, 794); // floor(39.7392 / 0.05)
        assert_eq!(c.col, -2100); // floor(-2099.8) = -2100
        assert_eq!(c.key(), "794:-2100");
    }

    #[test]
    fn cell_bbox_round_trips() {
        let c = Cell {
            row: 794,
            col: -2100,
        };
        let b = c.bbox();
        assert!((b.south - 39.70).abs() < 1e-9);
        assert!((b.north - 39.75).abs() < 1e-9);
        assert!((b.west + 105.0).abs() < 1e-9);
        assert!((b.east + 104.95).abs() < 1e-9);
        assert_eq!(Cell::containing(b.south + 0.001, b.west + 0.001), c);
    }

    #[test]
    fn cells_for_bbox_counts_cells() {
        // 0.12° tall × 0.12° wide starting on a cell boundary → 3 × 3 cells.
        let cells = cells_for_bbox(&BBox::new(39.70, -105.00, 39.82, -104.88));
        assert_eq!(cells.len(), 9);
        let set: BTreeSet<_> = cells.iter().collect();
        assert_eq!(set.len(), 9);
    }

    #[test]
    fn antimeridian_bbox_splits_into_two() {
        let b = BBox::new(-17.0, 178.0, -16.0, -179.0);
        assert!(b.crosses_antimeridian());
        let parts = b.split_antimeridian();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].east, 180.0);
        assert_eq!(parts[1].west, -180.0);
        assert!(b.contains(-16.5, 179.5));
        assert!(b.contains(-16.5, -179.5));
        assert!(!b.contains(-16.5, 0.0));
        // rows -340..=-321 (20) × cols (3560..=3599 → 40) + (-3600..=-3581 → 20) = 60
        let cells = cells_for_bbox(&b);
        assert_eq!(cells.len(), 20 * 60);
        assert!(cells.iter().any(|c| c.col == 3599), "east strip up to lon 180 is present");
        assert!(cells.iter().any(|c| c.col == -3600), "west strip from lon -180 is present");
        assert!(cells.iter().all(|c| c.col >= -3600 && c.col <= 3599));
    }

    #[test]
    fn union_bbox_covers_all_cells() {
        let cells = vec![Cell { row: 1, col: 1 }, Cell { row: 3, col: -2 }];
        let u = union_bbox(&cells).unwrap();
        assert!((u.south - 0.05).abs() < 1e-9);
        assert!((u.north - 0.20).abs() < 1e-9);
        assert!((u.west + 0.10).abs() < 1e-9);
        assert!((u.east - 0.10).abs() < 1e-9);
        assert!(union_bbox(&[]).is_none());
    }

    #[test]
    fn wrap_lon_wraps() {
        assert!((wrap_lon(190.0) + 170.0).abs() < 1e-9);
        assert!((wrap_lon(-190.0) - 170.0).abs() < 1e-9);
        assert!((wrap_lon(180.0) + 180.0).abs() < 1e-9);
        assert!((wrap_lon(45.0) - 45.0).abs() < 1e-9);
    }

    #[test]
    fn circle_cells_scale_with_latitude() {
        let equator = cells_for_circle(0.0, 0.0, 10_000.0).len();
        let high = cells_for_circle(70.0, 0.0, 10_000.0).len();
        assert!(high > equator, "high={high} equator={equator}");
    }

    #[test]
    fn polyline_cells_follow_the_line_not_its_rectangle() {
        // One diagonal segment across ~1° × 1° is bounded by its rectangle (≈ 20×20 cells).
        let pts = vec![(39.0, -105.0), (40.0, -104.0)];
        let coarse = cells_for_polyline(&pts, 100.0).len();
        assert!(coarse >= 400, "coarse={coarse}");
        // The same line densified: per-segment boxes hug the diagonal.
        let dense: Vec<(f64, f64)> = (0..=100)
            .map(|i| (39.0 + i as f64 / 100.0, -105.0 + i as f64 / 100.0))
            .collect();
        let fine = cells_for_polyline(&dense, 100.0).len();
        assert!(fine < 100, "fine={fine}");
    }

    #[test]
    fn sanitized_swaps_and_clamps() {
        let b = BBox::new(50.0, -10.0, 40.0, 10.0).sanitized();
        assert_eq!((b.south, b.north), (40.0, 50.0));
        let b = BBox::new(-95.0, -200.0, 95.0, 200.0).sanitized();
        assert_eq!(
            (b.south, b.north, b.west, b.east),
            (-90.0, 90.0, -180.0, 180.0)
        );
    }
}
