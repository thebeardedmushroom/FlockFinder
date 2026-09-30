//! The simulated location provider: drives along a route at a set speed with GPS noise, or
//! replays a GPX track, producing the same fixes a phone would.
//!
//! For development and tests (and the debug-only menu in the app). It can also leave the route
//! (a turn onto a side street, to exercise off-route and rerouting) and go quiet (a tunnel, to
//! exercise signal loss). Time is the simulator's own, so a drive can run faster than real time
//! and a test runs instantly.

use super::session::Fix;
use crate::geo_util::haversine_m;
use crate::gpx::TimedPoint;
use crate::routing::bearing;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SimParams {
    pub speed_mps: f64,
    /// Standard deviation of the position noise, metres.
    pub noise_m: f64,
    /// Simulated seconds per real second (the app only; tests run as fast as they can).
    pub rate: f64,
    pub seed: u64,
}

impl Default for SimParams {
    fn default() -> Self {
        SimParams { speed_mps: 13.4, noise_m: 4.0, rate: 1.0, seed: 7 }
    }
}

/// Leaving the route: straight on at `bearing` for `remaining_m`, then back to where it left.
#[derive(Debug, Clone, Copy)]
struct Detour {
    bearing: f64,
    remaining_m: f64,
    rejoin: (f64, f64),
    returning: bool,
}

/// Where the simulated car goes.
#[derive(Debug, Clone)]
enum Track {
    /// Along a path at the set speed.
    Drive { path: Vec<(f64, f64)>, cum: Vec<f64>, along: f64 },
    /// A recorded track: fix `next` is due at time offset `offsets[next]`.
    Replay { points: Vec<TimedPoint>, offsets: Vec<i64>, next: usize },
}

pub struct Simulator {
    pub params: SimParams,
    track: Track,
    /// Where the car really is (before noise).
    pos: (f64, f64),
    heading: f64,
    t_ms: i64,
    t0_ms: i64,
    rng: u64,
    detour: Option<Detour>,
    silent_until_ms: Option<i64>,
}

fn cumulative(path: &[(f64, f64)]) -> Vec<f64> {
    let mut cum = vec![0.0];
    for w in path.windows(2) {
        cum.push(cum.last().copied().unwrap_or(0.0) + haversine_m(w[0].0, w[0].1, w[1].0, w[1].1));
    }
    cum
}

fn point_at(path: &[(f64, f64)], cum: &[f64], along: f64) -> ((f64, f64), f64) {
    let i = cum.partition_point(|&c| c <= along).saturating_sub(1).min(path.len() - 2);
    let len = cum[i + 1] - cum[i];
    let t = if len > 0.0 { ((along - cum[i]) / len).clamp(0.0, 1.0) } else { 0.0 };
    let (a, b) = (path[i], path[i + 1]);
    ((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t), bearing(a, b))
}

/// `p` moved `m` metres towards `bearing_deg`.
pub fn offset(p: (f64, f64), bearing_deg: f64, m: f64) -> (f64, f64) {
    let b = bearing_deg.to_radians();
    let north = m * b.cos();
    let east = m * b.sin();
    (p.0 + north / 111_320.0, p.1 + east / (111_320.0 * p.0.to_radians().cos().max(0.01)))
}

impl Simulator {
    /// Drive `path` from `start_along` metres along it.
    pub fn drive(path: Vec<(f64, f64)>, start_along: f64, params: SimParams, t0_ms: i64) -> Self {
        let cum = cumulative(&path);
        let along = start_along.clamp(0.0, cum.last().copied().unwrap_or(0.0));
        let (pos, heading) = point_at(&path, &cum, along);
        Simulator {
            params,
            track: Track::Drive { path, cum, along },
            pos,
            heading,
            t_ms: t0_ms,
            t0_ms,
            rng: params.seed.max(1),
            detour: None,
            silent_until_ms: None,
        }
    }

    /// Replay a recorded track. Points without times are spaced at the set speed.
    pub fn replay(points: Vec<TimedPoint>, params: SimParams, t0_ms: i64) -> Self {
        let first_time = points.first().and_then(|p| p.time_ms);
        let mut offsets = Vec::with_capacity(points.len());
        let mut t = 0i64;
        for (i, p) in points.iter().enumerate() {
            t = match (p.time_ms, first_time) {
                (Some(pt), Some(f)) => pt - f,
                _ if i == 0 => 0,
                _ => {
                    let q = points[i - 1];
                    t + (haversine_m(q.lat, q.lon, p.lat, p.lon) / params.speed_mps.max(0.1) * 1000.0) as i64
                }
            };
            offsets.push(t);
        }
        let pos = points.first().map(|p| (p.lat, p.lon)).unwrap_or((0.0, 0.0));
        Simulator {
            params,
            track: Track::Replay { points, offsets, next: 0 },
            pos,
            heading: 0.0,
            t_ms: t0_ms,
            t0_ms,
            rng: params.seed.max(1),
            detour: None,
            silent_until_ms: None,
        }
    }

    pub fn now_ms(&self) -> i64 {
        self.t_ms
    }

    /// Follow a new route (after a reroute), from its point nearest the car.
    pub fn follow(&mut self, path: Vec<(f64, f64)>) {
        if let Track::Replay { .. } = self.track {
            return;
        }
        if path.len() < 2 {
            return;
        }
        let cum = cumulative(&path);
        let along = crate::geo_util::point_to_polyline(self.pos.0, self.pos.1, &path).map(|h| h.along_m).unwrap_or(0.0);
        self.detour = None;
        self.track = Track::Drive { path, cum, along };
    }

    /// Turn off the route (to the right) and drive `m` metres before heading back.
    pub fn deviate(&mut self, m: f64) {
        if let Track::Drive { .. } = self.track {
            self.detour = Some(Detour { bearing: (self.heading + 90.0).rem_euclid(360.0), remaining_m: m, rejoin: self.pos, returning: false });
        }
    }

    /// No fixes for `secs` seconds (the car keeps going).
    pub fn lose_signal(&mut self, secs: f64) {
        self.silent_until_ms = Some(self.t_ms + (secs * 1000.0) as i64);
    }

    pub fn set_speed(&mut self, mps: f64) {
        self.params.speed_mps = mps.max(0.0);
    }

    pub fn set_noise(&mut self, m: f64) {
        self.params.noise_m = m.max(0.0);
    }

    /// Reached the end of the path, or of the recording.
    #[cfg(test)]
    pub fn finished(&self) -> bool {
        match &self.track {
            Track::Drive { cum, along, .. } => self.detour.is_none() && *along >= cum.last().copied().unwrap_or(0.0),
            Track::Replay { points, next, .. } => *next >= points.len(),
        }
    }

    fn uniform(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gaussian(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Advance the clock by `dt_ms` and return the fix for the new time (none while the
    /// signal is lost).
    pub fn step(&mut self, dt_ms: i64) -> Option<Fix> {
        self.t_ms += dt_ms;
        let dist = self.params.speed_mps * dt_ms as f64 / 1000.0;
        let mut speed = self.params.speed_mps;
        match &mut self.track {
            Track::Replay { points, offsets, next } => {
                let elapsed = self.t_ms - self.t0_ms;
                let mut due = None;
                while *next < points.len() && offsets[*next] <= elapsed {
                    due = Some(*next);
                    *next += 1;
                }
                let i = due?;
                let p = points[i];
                let prev = if i > 0 { Some(points[i - 1]) } else { None };
                let (course, v) = match prev {
                    Some(q) => {
                        let d = haversine_m(q.lat, q.lon, p.lat, p.lon);
                        let dt = ((offsets[i] - offsets[i - 1]) as f64 / 1000.0).max(0.001);
                        ((d > 1.0).then(|| bearing((q.lat, q.lon), (p.lat, p.lon))), d / dt)
                    }
                    None => (None, 0.0),
                };
                self.pos = (p.lat, p.lon);
                if self.silent_until_ms.is_some_and(|t| self.t_ms < t) {
                    return None;
                }
                return Some(Fix { lat: p.lat, lon: p.lon, accuracy_m: 5.0, speed_mps: Some(v), course_deg: course, time_ms: self.t_ms });
            }
            Track::Drive { path, cum, along } => {
                if let Some(d) = &mut self.detour {
                    if !d.returning {
                        let go = dist.min(d.remaining_m);
                        self.pos = offset(self.pos, d.bearing, go);
                        self.heading = d.bearing;
                        d.remaining_m -= go;
                        if d.remaining_m <= 0.0 {
                            d.returning = true;
                        }
                    } else {
                        let left = haversine_m(self.pos.0, self.pos.1, d.rejoin.0, d.rejoin.1);
                        let b = bearing(self.pos, d.rejoin);
                        if left <= dist {
                            self.pos = d.rejoin;
                            self.detour = None;
                        } else {
                            self.pos = offset(self.pos, b, dist);
                        }
                        self.heading = b;
                    }
                } else {
                    let end = cum.last().copied().unwrap_or(0.0);
                    *along = (*along + dist).min(end);
                    if *along >= end {
                        speed = 0.0;
                    }
                    let (p, h) = point_at(path, cum, *along);
                    self.pos = p;
                    self.heading = h;
                }
            }
        }
        if self.silent_until_ms.is_some_and(|t| self.t_ms < t) {
            return None;
        }
        let noise = self.params.noise_m;
        let (n, e) = (self.gaussian() * noise, self.gaussian() * noise);
        let p = offset(offset(self.pos, 0.0, n), 90.0, e);
        let course_noise = if speed > 0.0 { self.gaussian() * 3.0 } else { 0.0 };
        Some(Fix {
            lat: p.0,
            lon: p.1,
            accuracy_m: (noise * 1.5).max(3.0),
            speed_mps: Some((speed + self.gaussian() * 0.3).max(0.0)),
            course_deg: (speed > 0.0).then(|| (self.heading + course_noise).rem_euclid(360.0)),
            time_ms: self.t_ms,
        })
    }
}

/// Fixes as a GPX track (for recording test drives).
#[cfg(test)]
pub fn to_gpx(fixes: &[Fix]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"Flock Finder simulator\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n<trk><trkseg>\n",
    );
    for f in fixes {
        let t = chrono::DateTime::from_timestamp_millis(f.time_ms).unwrap_or_default();
        s.push_str(&format!(
            "<trkpt lat=\"{:.7}\" lon=\"{:.7}\"><time>{}</time></trkpt>\n",
            f.lat,
            f.lon,
            t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        ));
    }
    s.push_str("</trkseg></trk>\n</gpx>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_the_path_at_the_set_speed_and_stops_at_the_end() {
        let path = vec![(39.0, -105.0), (39.01, -105.0)];
        let mut sim = Simulator::drive(path, 0.0, SimParams { speed_mps: 10.0, noise_m: 0.0, ..Default::default() }, 0);
        let f = sim.step(1000).unwrap();
        assert!((haversine_m(39.0, -105.0, f.lat, f.lon) - 10.0).abs() < 0.1);
        assert!(f.course_deg.unwrap().abs() < 5.0 || f.course_deg.unwrap() > 355.0);
        for _ in 0..200 {
            sim.step(1000);
        }
        assert!(sim.finished());
    }

    #[test]
    fn a_detour_leaves_the_path_and_comes_back() {
        let path = vec![(39.0, -105.0), (39.02, -105.0)];
        let mut sim = Simulator::drive(path, 500.0, SimParams { speed_mps: 10.0, noise_m: 0.0, ..Default::default() }, 0);
        sim.deviate(200.0);
        let mut max_off: f64 = 0.0;
        for _ in 0..60 {
            let f = sim.step(1000).unwrap();
            max_off = max_off.max((f.lon + 105.0).abs() * 111_320.0 * 39f64.to_radians().cos());
        }
        assert!((max_off - 200.0).abs() < 5.0, "{max_off}");
        assert!(sim.detour.is_none(), "back on the path");
    }

    #[test]
    fn signal_loss_drops_fixes_for_the_duration() {
        let path = vec![(39.0, -105.0), (39.02, -105.0)];
        let mut sim = Simulator::drive(path, 0.0, SimParams::default(), 0);
        sim.lose_signal(3.0);
        assert!(sim.step(1000).is_none());
        assert!(sim.step(1000).is_none());
        assert!(sim.step(1000).is_some());
    }

    #[test]
    fn gpx_round_trip_keeps_times() {
        let fixes = vec![
            Fix { lat: 39.0, lon: -105.0, accuracy_m: 5.0, speed_mps: None, course_deg: None, time_ms: 1_000_000 },
            Fix { lat: 39.001, lon: -105.0, accuracy_m: 5.0, speed_mps: None, course_deg: None, time_ms: 1_001_500 },
        ];
        let pts = crate::gpx::parse_points(&to_gpx(&fixes)).unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[1].time_ms, Some(1_001_500));
        let mut sim = Simulator::replay(pts, SimParams::default(), 0);
        assert!(sim.step(0).is_some());
        assert!(sim.step(1000).is_none(), "the next point is due at 1.5 s");
        let f = sim.step(500).unwrap();
        assert!((f.speed_mps.unwrap() - 111.2 / 1.5).abs() < 1.0);
    }
}
