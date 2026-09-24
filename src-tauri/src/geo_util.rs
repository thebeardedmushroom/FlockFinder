//! Spherical geometry helpers built on the `geo` crate.

use geo::{Closest, Coord, Distance, Haversine, HaversineClosestPoint, Line, Point};
use serde::Serialize;

/// Great-circle distance in metres between two (lat, lon) points.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    Haversine.distance(Point::new(lon1, lat1), Point::new(lon2, lat2))
}

pub fn valid_coord(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct PolylineHit {
    /// Perpendicular (great-circle) distance from the point to the polyline, metres.
    pub distance_m: f64,
    /// Distance along the polyline from its first vertex to the closest point, metres.
    pub along_m: f64,
    /// Index of the segment (0-based) that contains the closest point.
    pub segment: usize,
    pub closest_lat: f64,
    pub closest_lon: f64,
}

/// Total length of a (lat, lon) polyline in metres.
pub fn polyline_length_m(line: &[(f64, f64)]) -> f64 {
    line.windows(2)
        .map(|w| haversine_m(w[0].0, w[0].1, w[1].0, w[1].1))
        .sum()
}

/// Closest approach of a point to a (lat, lon) polyline. `None` if the polyline is empty.
pub fn point_to_polyline(lat: f64, lon: f64, line: &[(f64, f64)]) -> Option<PolylineHit> {
    if line.is_empty() {
        return None;
    }
    let pt = Point::new(lon, lat);
    if line.len() == 1 {
        let d = haversine_m(lat, lon, line[0].0, line[0].1);
        return Some(PolylineHit {
            distance_m: d,
            along_m: 0.0,
            segment: 0,
            closest_lat: line[0].0,
            closest_lon: line[0].1,
        });
    }

    let mut best: Option<PolylineHit> = None;
    let mut cumulative = 0.0;
    for (i, w) in line.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        let seg = Line::new(Coord { x: a.1, y: a.0 }, Coord { x: b.1, y: b.0 });
        let closest = match seg.haversine_closest_point(&pt) {
            Closest::Intersection(p) | Closest::SinglePoint(p) => p,
            Closest::Indeterminate => {
                // Degenerate zero-length segment: fall back to its start vertex.
                Point::new(a.1, a.0)
            }
        };
        let d = Haversine.distance(pt, closest);
        if best.map_or(true, |h| d < h.distance_m) {
            best = Some(PolylineHit {
                distance_m: d,
                along_m: cumulative + haversine_m(a.0, a.1, closest.y(), closest.x()),
                segment: i,
                closest_lat: closest.y(),
                closest_lon: closest.x(),
            });
        }
        cumulative += haversine_m(a.0, a.1, b.0, b.1);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_one_degree_of_latitude() {
        let d = haversine_m(0.0, 0.0, 1.0, 0.0);
        assert!((d - 111_195.0).abs() < 200.0, "d={d}");
    }

    #[test]
    fn haversine_known_city_pair() {
        // London (51.5074, -0.1278) → Paris (48.8566, 2.3522) ≈ 343.5 km
        let d = haversine_m(51.5074, -0.1278, 48.8566, 2.3522);
        assert!((d - 343_500.0).abs() < 1_500.0, "d={d}");
    }

    #[test]
    fn haversine_is_symmetric_and_zero_at_identity() {
        assert_eq!(haversine_m(39.0, -105.0, 39.0, -105.0), 0.0);
        let a = haversine_m(39.0, -105.0, 40.0, -104.0);
        let b = haversine_m(40.0, -104.0, 39.0, -105.0);
        assert!((a - b).abs() < 1e-6);
    }

    #[test]
    fn haversine_across_antimeridian() {
        // 0.1° of longitude straddling ±180 at the equator ≈ 11.1 km, not 40,000 km.
        let d = haversine_m(0.0, 179.95, 0.0, -179.95);
        assert!((d - 11_120.0).abs() < 100.0, "d={d}");
    }

    #[test]
    fn point_to_polyline_perpendicular_distance() {
        // North–south segment along lon 0 from lat 0 to lat 1. Point ~100 m east at lat 0.5.
        let line = vec![(0.0, 0.0), (1.0, 0.0)];
        let lon_100m = 100.0 / 111_320.0;
        let hit = point_to_polyline(0.5, lon_100m, &line).unwrap();
        assert!((hit.distance_m - 100.0).abs() < 1.0, "dist={}", hit.distance_m);
        assert!((hit.along_m - 55_597.0).abs() < 100.0, "along={}", hit.along_m);
        assert_eq!(hit.segment, 0);
    }

    #[test]
    fn point_beyond_endpoint_snaps_to_endpoint() {
        let line = vec![(0.0, 0.0), (1.0, 0.0)];
        let hit = point_to_polyline(1.01, 0.0, &line).unwrap();
        let expected = haversine_m(1.01, 0.0, 1.0, 0.0);
        assert!((hit.distance_m - expected).abs() < 1.0);
        assert!((hit.closest_lat - 1.0).abs() < 1e-9);
        let total = polyline_length_m(&line);
        assert!((hit.along_m - total).abs() < 1.0);
    }

    #[test]
    fn point_to_polyline_picks_nearest_segment() {
        // L-shaped route: north along lon 0, then east along lat 1.
        let line = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)];
        let hit = point_to_polyline(1.001, 0.7, &line).unwrap();
        assert_eq!(hit.segment, 1);
        assert!(hit.distance_m < 200.0);
        let along_expected = haversine_m(0.0, 0.0, 1.0, 0.0) + haversine_m(1.0, 0.0, 1.0, 0.7);
        assert!((hit.along_m - along_expected).abs() < 200.0);
    }

    #[test]
    fn point_to_polyline_handles_degenerate_input() {
        assert!(point_to_polyline(0.0, 0.0, &[]).is_none());
        let single = point_to_polyline(0.0, 0.001, &[(0.0, 0.0)]).unwrap();
        assert!((single.distance_m - 111.3).abs() < 1.0);
        // Zero-length segment does not panic.
        let zero = point_to_polyline(0.0, 0.001, &[(0.0, 0.0), (0.0, 0.0)]).unwrap();
        assert!((zero.distance_m - 111.3).abs() < 1.0);
    }

    #[test]
    fn coordinate_validation() {
        assert!(valid_coord(0.0, 0.0));
        assert!(valid_coord(-90.0, 180.0));
        assert!(!valid_coord(90.1, 0.0));
        assert!(!valid_coord(0.0, -180.1));
        assert!(!valid_coord(f64::NAN, 0.0));
    }
}
