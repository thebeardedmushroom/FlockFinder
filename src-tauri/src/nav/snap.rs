//! Matching a position to the route: where along it the car is, and how far off it.
//!
//! A raw fix is matched to the nearest route segment, but only among segments near the
//! progress so far (a window from a little behind to a speed-dependent distance ahead), and a
//! segment pointing the wrong way for the direction of travel scores worse. Together these keep
//! the match from jumping to a parallel road, the other carriageway, or the road on an
//! overpass that the route crosses later.

use super::route::NavRoute;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    /// Distance along the route of the matched point, metres.
    pub along_m: f64,
    /// Distance from the fix to the matched point, metres.
    pub dist_m: f64,
    pub segment: usize,
    /// The matched point (lat, lon).
    pub point: (f64, f64),
    /// Direction of the route there.
    pub bearing: f64,
}

/// Where to look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Window {
    /// Around the progress so far: from `behind` metres back to `ahead` metres on.
    Around { along: f64, behind: f64, ahead: f64 },
    /// The whole route (the first fix, or finding the route again after leaving it).
    Whole,
}

/// A segment pointing more than this far from the direction of travel costs extra (metres of
/// score), in proportion up to…
const HEADING_FREE_DEG: f64 = 45.0;
/// …this at 135° or more (the other way).
const HEADING_PENALTY_M: f64 = 60.0;
/// Each metre behind the progress so far costs this much (jitter backwards is fine, a jump is
/// not).
const BACKWARD_COST: f64 = 0.5;

/// Smallest angle between two bearings, degrees.
pub fn angle_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// Closest point of segment `i` to `p`: (fraction along it, distance in metres), using a local
/// flat projection (accurate at the scale of a road segment).
fn project(route: &NavRoute, i: usize, p: (f64, f64)) -> (f64, f64) {
    let (a, b) = (route.shape[i], route.shape[i + 1]);
    let cos = p.0.to_radians().cos().max(0.01);
    let xy = |q: (f64, f64)| ((q.1 - p.1) * 111_320.0 * cos, (q.0 - p.0) * 111_320.0);
    let (ax, ay) = xy(a);
    let (bx, by) = xy(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (t, (ax + t * dx).hypot(ay + t * dy))
}

/// Match `p` to the route. `course`: direction of travel, when known and trustworthy (moving).
pub fn snap(route: &NavRoute, p: (f64, f64), course: Option<f64>, window: Window) -> Snap {
    let last_seg = route.shape.len() - 2;
    let (range, progress) = match window {
        Window::Around { along, behind, ahead } => {
            (route.segment_at((along - behind).max(0.0))..=route.segment_at(along + ahead), Some(along))
        }
        Window::Whole => (0..=last_seg, None),
    };
    let mut best: Option<(f64, Snap)> = None;
    for i in range {
        let (t, dist) = project(route, i, p);
        let seg_len = route.cum[i + 1] - route.cum[i];
        let along = route.cum[i] + t * seg_len;
        let seg_bearing = route.segment_bearing(i);
        let mut score = dist;
        if let Some(c) = course {
            let diff = angle_diff(c, seg_bearing);
            if diff > HEADING_FREE_DEG {
                score += HEADING_PENALTY_M * ((diff - HEADING_FREE_DEG) / 90.0).min(1.0);
            }
        }
        if let Some(prev) = progress {
            if along < prev {
                score += (prev - along) * BACKWARD_COST;
            }
        }
        if best.as_ref().map_or(true, |(s, _)| score < *s) {
            let (a, b) = (route.shape[i], route.shape[i + 1]);
            let point = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            best = Some((score, Snap { along_m: along, dist_m: dist, segment: i, point, bearing: seg_bearing }));
        }
    }
    best.expect("a route has at least one segment").1
}

#[cfg(test)]
mod tests {
    use super::super::route::testing::route_through;
    use super::super::route::Mode;
    use super::*;

    fn offset(p: (f64, f64), north_m: f64, east_m: f64) -> (f64, f64) {
        (p.0 + north_m / 111_320.0, p.1 + east_m / (111_320.0 * p.0.to_radians().cos()))
    }

    #[test]
    fn snaps_to_the_nearest_point_with_distance() {
        let r = route_through(&[(39.0, -105.0), (39.01, -105.0)], Mode::Fastest);
        let s = snap(&r, offset((39.005, -105.0), 0.0, 25.0), None, Window::Whole);
        assert!((s.dist_m - 25.0).abs() < 0.5, "{}", s.dist_m);
        assert!((s.along_m - 556.0).abs() < 2.0, "{}", s.along_m);
    }

    #[test]
    fn an_out_and_back_route_matches_the_side_you_are_driving() {
        // North 1 km, then back south 1 km on a road 30 m to the east (a divided road).
        let a = (39.0, -105.0);
        let b = offset(a, 1000.0, 0.0);
        let c = offset(b, 0.0, 30.0);
        let d = offset(a, 0.0, 30.0);
        let r = route_through(&[a, b, c, d], Mode::Fastest);
        // 500 m north, between the carriageways, heading south: the southbound side.
        let p = offset(a, 500.0, 14.0);
        let s = snap(&r, p, Some(180.0), Window::Whole);
        assert!(s.along_m > 1000.0, "matched the northbound side: {}", s.along_m);
        let s = snap(&r, p, Some(0.0), Window::Whole);
        assert!(s.along_m < 1000.0, "matched the southbound side: {}", s.along_m);
    }

    #[test]
    fn the_window_keeps_a_crossing_later_on_the_route_from_stealing_the_match() {
        // A loop: east 1 km, north, west, then south crossing the first leg's start area
        // (an overpass 5 m away from the point being driven).
        let a = (39.0, -105.0);
        let pts = [a, offset(a, 0.0, 1000.0), offset(a, 500.0, 1000.0), offset(a, 500.0, 205.0), offset(a, -500.0, 205.0)];
        let r = route_through(&pts, Mode::Fastest);
        let p = offset(a, 3.0, 200.0); // on the first leg, 5 m from the crossing road
        let s = snap(&r, p, Some(90.0), Window::Around { along: 190.0, behind: 50.0, ahead: 300.0 });
        assert!((s.along_m - 200.0).abs() < 5.0, "{}", s.along_m);
    }
}
