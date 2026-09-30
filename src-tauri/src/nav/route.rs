//! A route prepared for guidance: the shape with distances along it, and the maneuvers and
//! cameras placed on it.

use crate::geo_util::haversine_m;
use crate::routing::{bearing, Maneuver, PlannedRoute, RouteCamera};
use serde::{Deserialize, Serialize};

/// Which route of a plan is being followed; a reroute keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Avoid,
    Fastest,
}

/// Maneuver types (Valhalla's numbering) that start or end a route.
pub fn is_start(kind: u32) -> bool {
    (1..=3).contains(&kind)
}

pub fn is_arrival(kind: u32) -> bool {
    (4..=6).contains(&kind)
}

/// The icon for a maneuver type (Valhalla's numbering); the app and the Android notification
/// both have one drawing per name.
pub fn maneuver_icon(kind: u32) -> &'static str {
    match kind {
        1..=3 => "depart",
        4..=6 => "arrive",
        9 => "slight_right",
        10 => "right",
        11 => "sharp_right",
        12 | 13 => "uturn",
        14 => "sharp_left",
        15 => "left",
        16 => "slight_left",
        18 | 20 | 23 => "keep_right",
        19 | 21 | 24 => "keep_left",
        25 | 37 | 38 => "merge",
        26 | 27 => "roundabout",
        28 | 29 => "ferry",
        _ => "straight",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NavRoute {
    pub mode: Mode,
    /// (lat, lon)
    pub shape: Vec<(f64, f64)>,
    /// Distance along the route at each shape point, metres.
    pub cum: Vec<f64>,
    pub maneuvers: Vec<Maneuver>,
    /// Distance along the route where each maneuver begins.
    pub maneuver_at: Vec<f64>,
    /// Cameras the route passes, in driving order, `along_m` measured on `shape`.
    pub cameras: Vec<RouteCamera>,
    pub duration_s: f64,
}

impl NavRoute {
    pub fn new(mode: Mode, planned: &PlannedRoute) -> Option<Self> {
        let shape: Vec<(f64, f64)> = planned.shape.iter().map(|p| (p[0], p[1])).collect();
        if shape.len() < 2 {
            return None;
        }
        let mut cum = Vec::with_capacity(shape.len());
        cum.push(0.0);
        for w in shape.windows(2) {
            cum.push(cum.last().copied().unwrap_or(0.0) + haversine_m(w[0].0, w[0].1, w[1].0, w[1].1));
        }
        let last = shape.len() - 1;
        let maneuver_at = planned.maneuvers.iter().map(|m| cum[m.shape_index.min(last)]).collect();
        Some(NavRoute {
            mode,
            shape,
            cum,
            maneuvers: planned.maneuvers.clone(),
            maneuver_at,
            cameras: planned.cameras.clone(),
            duration_s: planned.duration_s,
        })
    }

    pub fn length_m(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    pub fn destination(&self) -> (f64, f64) {
        self.shape[self.shape.len() - 1]
    }

    /// Direction of segment `i` (shape point `i` to `i + 1`).
    pub fn segment_bearing(&self, i: usize) -> f64 {
        let i = i.min(self.shape.len() - 2);
        bearing(self.shape[i], self.shape[i + 1])
    }

    /// Index of the segment holding the point `along` metres from the start.
    pub fn segment_at(&self, along: f64) -> usize {
        let i = self.cum.partition_point(|&c| c <= along);
        i.saturating_sub(1).min(self.shape.len() - 2)
    }

    /// The point `along` metres from the start (clamped to the route).
    pub fn point_at(&self, along: f64) -> (f64, f64) {
        let along = along.clamp(0.0, self.length_m());
        let i = self.segment_at(along);
        let len = self.cum[i + 1] - self.cum[i];
        let t = if len > 0.0 { (along - self.cum[i]) / len } else { 0.0 };
        let (a, b) = (self.shape[i], self.shape[i + 1]);
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    /// The next maneuver strictly ahead of `along` (a maneuver counts as done once reached).
    pub fn next_maneuver(&self, along: f64) -> Option<usize> {
        self.maneuver_at.iter().position(|&m| m > along + 1.0)
    }

    /// Time left to drive from `along`: the maneuvers' own times, the current one pro rata.
    pub fn remaining_s(&self, along: f64) -> f64 {
        let Some(next) = self.next_maneuver(along) else { return 0.0 };
        let mut total: f64 = self.maneuvers[next..].iter().map(|m| m.duration_s).sum();
        if next > 0 {
            let (from, to) = (self.maneuver_at[next - 1], self.maneuver_at[next]);
            let span = to - from;
            if span > 0.0 {
                total += self.maneuvers[next - 1].duration_s * ((to - along) / span).clamp(0.0, 1.0);
            }
        }
        total
    }

    /// Cameras ahead of `along`.
    pub fn cameras_ahead(&self, along: f64) -> usize {
        self.cameras.iter().filter(|c| c.along_m > along).count()
    }

}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::grid::BBox;

    /// A straight-ish test route through `points` with a maneuver at each interior point.
    pub fn route_through(points: &[(f64, f64)], mode: Mode) -> NavRoute {
        let mut maneuvers = vec![Maneuver { instruction: "Drive.".into(), kind: 1, ..Default::default() }];
        for i in 1..points.len() - 1 {
            maneuvers.push(Maneuver {
                instruction: format!("Turn {i}."),
                kind: 10,
                shape_index: i,
                verbal_alert: Some(format!("Turn {i}.")),
                verbal_pre: Some(format!("Turn {i} now.")),
                ..Default::default()
            });
        }
        maneuvers.push(Maneuver {
            instruction: "Arrive.".into(),
            kind: 4,
            shape_index: points.len() - 1,
            verbal_alert: Some("You will arrive.".into()),
            verbal_pre: Some("You have arrived.".into()),
            ..Default::default()
        });
        let planned = PlannedRoute {
            shape: points.iter().map(|&(a, b)| [a, b]).collect(),
            distance_m: 0.0,
            duration_s: 600.0,
            cameras: Vec::new(),
            maneuvers,
            bbox: BBox::new(0.0, 0.0, 0.0, 0.0),
        };
        let mut r = NavRoute::new(mode, &planned).unwrap();
        // Times proportional to distance at 15 m/s.
        let at = r.maneuver_at.clone();
        for (i, m) in r.maneuvers.iter_mut().enumerate() {
            let end = at.get(i + 1).copied().unwrap_or(at[i]);
            m.distance_m = end - at[i];
            m.duration_s = m.distance_m / 15.0;
        }
        r.duration_s = r.maneuvers.iter().map(|m| m.duration_s).sum();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::testing::route_through;
    use super::*;

    #[test]
    fn distances_and_maneuvers_along_the_route() {
        // ~1.11 km north, then ~0.92 km east.
        let r = route_through(&[(39.0, -105.0), (39.01, -105.0), (39.01, -104.99)], Mode::Avoid);
        assert!((r.cum[1] - 1112.0).abs() < 5.0, "{}", r.cum[1]);
        assert_eq!(r.maneuver_at.len(), 3);
        assert_eq!(r.next_maneuver(0.0), Some(1));
        assert_eq!(r.next_maneuver(1200.0), Some(2));
        assert_eq!(r.next_maneuver(r.length_m()), None);
        let mid = r.point_at(556.0);
        assert!((mid.0 - 39.005).abs() < 1e-4);
        // Half the first step left, plus all of the second.
        let rem = r.remaining_s(r.cum[1] / 2.0);
        let expect = r.maneuvers[0].duration_s / 2.0 + r.maneuvers[1].duration_s;
        assert!((rem - expect).abs() < 1.0, "{rem} vs {expect}");
    }
}
