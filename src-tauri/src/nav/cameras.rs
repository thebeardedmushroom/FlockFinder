//! Camera alerts while navigating: cameras on the route ahead, at about a quarter mile and
//! again at 500 feet.
//!
//! Only cameras the route passes (within the avoidance radius) are alerted, by distance along
//! the route, so a camera behind the car or on a road the route doesn't take never is. Each
//! camera alerts at each distance at most once per trip, across reroutes, and the keys of the
//! cameras this covers are handed to the map's own proximity alerts so they stay quiet about
//! them.

use super::guidance::spoken_distance;
use super::route::NavRoute;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub const FAR_M: f64 = 402.3; // a quarter mile
pub const NEAR_M: f64 = 152.4; // 500 ft
/// Cameras this close together along the route are announced together.
const GROUP_M: f64 = 150.0;
/// A camera this far behind the car has been passed.
const PASSED_M: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertStage {
    Far,
    Near,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertCamera {
    pub key: String,
    pub lat: f64,
    pub lon: f64,
    /// `osm` (mapped) or `submission` (recorded in this app, unverified).
    pub source: String,
    pub category: String,
    pub operator: Option<String>,
    /// Distance ahead along the route, metres.
    pub distance_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CameraAlert {
    pub stage: AlertStage,
    /// Nearest first.
    pub cameras: Vec<AlertCamera>,
    /// What to say ("Camera ahead in a quarter mile.").
    pub text: String,
}

#[derive(Debug, Default, Clone)]
pub struct CameraAlerts {
    done: HashSet<(String, AlertStage)>,
    /// Cameras passed while navigating (key → where), for the arrival summary.
    passed: HashMap<String, (f64, f64)>,
}

impl CameraAlerts {
    pub fn passed(&self) -> usize {
        self.passed.len()
    }

    /// Everything a route's alerts cover (for the map's proximity alerts to skip).
    pub fn covered(route: &NavRoute) -> Vec<String> {
        route.cameras.iter().map(|c| c.camera.key.clone()).collect()
    }

    /// Cameras behind the car when guidance starts (or a new route is taken) don't count as
    /// passed on this trip, and are never alerted.
    pub fn skip_behind(&mut self, route: &NavRoute, along: f64) {
        for c in route.cameras.iter().filter(|c| c.along_m <= along) {
            self.done.insert((c.camera.key.clone(), AlertStage::Far));
            self.done.insert((c.camera.key.clone(), AlertStage::Near));
        }
    }

    /// Progress moved from `from` to `to` metres along `route`: count cameras passed and return
    /// the alert due, if any.
    pub fn update(&mut self, route: &NavRoute, from: f64, to: f64) -> Option<CameraAlert> {
        for c in &route.cameras {
            if c.along_m > from - PASSED_M && c.along_m <= to - PASSED_M {
                self.passed.entry(c.camera.key.clone()).or_insert((c.camera.lat, c.camera.lon));
            }
        }
        // The nearest stage due for the nearest camera not yet alerted at it.
        let mut due: Option<(AlertStage, f64)> = None;
        for c in route.cameras.iter().filter(|c| c.along_m > to) {
            let ahead = c.along_m - to;
            let stage = if ahead <= NEAR_M {
                AlertStage::Near
            } else if ahead <= FAR_M {
                AlertStage::Far
            } else {
                break;
            };
            if !self.done.contains(&(c.camera.key.clone(), stage)) {
                due = Some((stage, c.along_m));
                break;
            }
        }
        let (stage, first) = due?;
        // That camera and any just after it, announced together.
        let group: Vec<AlertCamera> = route
            .cameras
            .iter()
            .filter(|c| c.along_m >= first && c.along_m <= first + GROUP_M)
            .filter(|c| !self.done.contains(&(c.camera.key.clone(), stage)))
            .map(|c| AlertCamera {
                key: c.camera.key.clone(),
                lat: c.camera.lat,
                lon: c.camera.lon,
                source: c.camera.source.clone(),
                category: c.camera.category.clone(),
                operator: c.camera.operator.clone(),
                distance_m: c.along_m - to,
            })
            .collect();
        for c in &group {
            self.done.insert((c.key.clone(), stage));
            if stage == AlertStage::Near {
                self.done.insert((c.key.clone(), AlertStage::Far));
            }
        }
        let n = group.len();
        let unverified = group.iter().all(|c| c.source == "submission");
        let what = match (n, unverified) {
            (1, true) => "Unverified camera ahead".to_string(),
            (1, false) => "Camera ahead".to_string(),
            (_, _) => format!("{n} cameras ahead"),
        };
        let text = format!("{what} in {}.", spoken_distance(group[0].distance_m));
        Some(CameraAlert { stage, cameras: group, text })
    }
}

#[cfg(test)]
mod tests {
    use super::super::route::testing::route_through;
    use super::super::route::Mode;
    use super::*;
    use crate::routing::{AvoidCamera, RouteCamera};

    fn cam(key: &str, along: f64, source: &str) -> RouteCamera {
        RouteCamera {
            camera: AvoidCamera {
                key: key.into(),
                lat: 0.0,
                lon: 0.0,
                category: "flock".into(),
                source: source.into(),
                direction: None,
                operator: None,
            },
            along_m: along,
            distance_m: 5.0,
            remaining: None,
        }
    }

    fn route() -> NavRoute {
        let mut r = route_through(&[(39.0, -105.0), (39.03, -105.0)], Mode::Fastest);
        r.cameras = vec![cam("node/1", 1000.0, "osm"), cam("node/2", 1080.0, "osm"), cam("submission/3", 2500.0, "submission")];
        r
    }

    #[test]
    fn alerts_at_a_quarter_mile_and_500_feet_once_each() {
        let r = route();
        let mut a = CameraAlerts::default();
        let mut got = Vec::new();
        let mut along = 0.0;
        while along < 3000.0 {
            if let Some(al) = a.update(&r, along, along + 10.0) {
                got.push((along + 10.0, al));
            }
            along += 10.0;
        }
        let summary: Vec<(AlertStage, usize, String)> = got.iter().map(|(_, a)| (a.stage, a.cameras.len(), a.text.clone())).collect();
        assert_eq!(
            summary,
            vec![
                (AlertStage::Far, 2, "2 cameras ahead in a quarter mile.".to_string()),
                (AlertStage::Near, 2, "2 cameras ahead in 500 feet.".to_string()),
                (AlertStage::Far, 1, "Unverified camera ahead in a quarter mile.".to_string()),
                (AlertStage::Near, 1, "Unverified camera ahead in 500 feet.".to_string()),
            ]
        );
        assert!((got[0].0 - (1000.0 - FAR_M)).abs() <= 10.0);
        assert_eq!(a.passed(), 3);
    }

    #[test]
    fn cameras_behind_at_the_start_are_never_alerted_or_counted() {
        let r = route();
        let mut a = CameraAlerts::default();
        a.skip_behind(&r, 1050.0);
        // node/2 is 30 m ahead: 500 ft stage only.
        let al = a.update(&r, 1050.0, 1051.0).unwrap();
        assert_eq!(al.cameras.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), vec!["node/2"]);
        assert_eq!(al.stage, AlertStage::Near);
        assert_eq!(a.passed(), 0);
    }

    #[test]
    fn a_reroute_through_the_same_camera_does_not_alert_it_again() {
        let r = route();
        let mut a = CameraAlerts::default();
        let _ = a.update(&r, 0.0, 700.0);
        let _ = a.update(&r, 700.0, 900.0);
        // A new route with the same cameras, measured from a different start.
        let mut r2 = route();
        for c in &mut r2.cameras {
            c.along_m -= 500.0;
        }
        assert!(a.update(&r2, 400.0, 410.0).is_none(), "already alerted at both distances");
    }
}
