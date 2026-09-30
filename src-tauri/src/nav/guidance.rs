//! What to say, and when.
//!
//! The words come from the routing server: each maneuver carries text written for speech
//! (`verbal_alert`, a short form, and `verbal_pre`, the full form). This module only decides
//! when to say them and adds the distance ("In a half mile, …"):
//!
//! - **Far**: about 1 mile ahead on a highway, half a mile on other roads.
//! - **Near**: about 500 feet ahead.
//! - **Now**: at the maneuver, a few seconds before reaching it.
//!
//! Each is said once per maneuver. When several are due at once (the maneuver came up
//! quickly, or the position jumped after a gap), only the latest is said and the others are
//! dropped, so nothing out of date is ever spoken. A stage is skipped when there isn't time
//! to say it before the next one (the 500 ft prompt at highway speed, say).

use super::route::{is_arrival, is_start, NavRoute};
use crate::routing::METERS_PER_MILE;
use serde::Serialize;
use std::collections::HashSet;

pub const FAR_HIGHWAY_M: f64 = METERS_PER_MILE;
pub const FAR_SURFACE_M: f64 = METERS_PER_MILE / 2.0;
pub const NEAR_M: f64 = 152.4; // 500 ft
/// "Now" is said this many seconds of driving before the maneuver, within these bounds.
const NOW_SECS: f64 = 3.5;
const NOW_MIN_M: f64 = 30.0;
const NOW_MAX_M: f64 = 250.0;
/// Seconds it takes to say a prompt; a stage is skipped when the next is due sooner.
const SPEAK_SECS: f64 = 4.0;
/// Below this the car is taken to be crawling: timing uses this speed instead.
const MIN_TIMING_SPEED: f64 = 5.0;
/// A new step this much longer than the far distance gets "Continue for …" after the turn.
const POST_MARGIN_M: f64 = 300.0;
/// Show the maneuver after the next one when it follows within this distance.
pub const THEN_WITHIN_M: f64 = 150.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// "Continue for 2 miles." after a maneuver.
    Post,
    Far,
    Near,
    Now,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prompt {
    pub text: String,
    pub maneuver: usize,
    pub stage: Stage,
}

/// The distance as spoken: "500 feet", "a quarter mile", "a half mile", "1 mile", "3 miles".
pub fn spoken_distance(m: f64) -> String {
    let mi = m / METERS_PER_MILE;
    if mi < 0.19 {
        let ft = ((m * 3.28084 / 100.0).round() * 100.0).max(100.0);
        return format!("{ft:.0} feet");
    }
    let quarters = (mi * 4.0).round();
    if mi < 1.13 {
        return match quarters as i64 {
            1 => "a quarter mile".into(),
            2 => "a half mile".into(),
            3 => "three quarters of a mile".into(),
            _ => "1 mile".into(),
        };
    }
    let whole = mi.round();
    if (mi - whole).abs() < 0.13 || mi >= 10.0 {
        format!("{whole:.0} miles")
    } else {
        format!("{:.1} miles", mi)
    }
}

/// Where each stage is due for a maneuver, metres before it, at `speed` m/s. `None`: skipped.
fn thresholds(route: &NavRoute, next: usize, speed: f64) -> [(Stage, Option<f64>); 3] {
    let v = speed.max(MIN_TIMING_SPEED);
    let on_highway = next > 0 && route.maneuvers[next - 1].highway;
    let far = if on_highway { FAR_HIGHWAY_M } else { FAR_SURFACE_M };
    let now = (v * NOW_SECS).clamp(NOW_MIN_M, NOW_MAX_M);
    let near = (NEAR_M - now >= v * SPEAK_SECS).then_some(NEAR_M);
    [(Stage::Far, Some(far)), (Stage::Near, near), (Stage::Now, Some(now))]
}

fn with_distance(dist: f64, text: &str) -> String {
    format!("In {}, {}", spoken_distance(dist), text)
}

/// Remembers what was said (or skipped) for the current route.
#[derive(Debug, Default, Clone)]
pub struct Prompter {
    done: HashSet<(usize, Stage)>,
    /// The maneuver that was next at the last update.
    last_next: Option<usize>,
}

impl Prompter {
    /// Forget everything (a new route).
    pub fn reset(&mut self) {
        *self = Prompter::default();
    }

    /// The first thing said when guidance starts: the start maneuver's full text ("Drive west.
    /// Then, in 200 feet, Turn right.").
    pub fn opening(&mut self, route: &NavRoute) -> Option<Prompt> {
        let first = route.maneuvers.first()?;
        self.done.insert((0, Stage::Now));
        let text = first.verbal_pre.clone().unwrap_or_else(|| first.instruction.clone());
        // "…Then, in 200 feet, Turn right.": the next maneuver's advance notice is given.
        if text.contains("Then") {
            self.done.insert((1, Stage::Far));
            self.done.insert((1, Stage::Near));
        }
        Some(Prompt { text, maneuver: 0, stage: Stage::Now })
    }

    /// The prompt due at `along` metres (at most one: the latest due), or none.
    pub fn update(&mut self, route: &NavRoute, along: f64, speed: f64) -> Option<Prompt> {
        let next = route.next_maneuver(along)?;
        let changed = self.last_next != Some(next);
        self.last_next = Some(next);
        let m = &route.maneuvers[next];
        if is_start(m.kind) {
            return None;
        }
        let dist = route.maneuver_at[next] - along;
        let stages = thresholds(route, next, speed);
        // The latest stage that is due and not yet done; everything before it is dropped.
        let mut due: Option<(Stage, f64)> = None;
        for (stage, at) in stages {
            if let Some(at) = at {
                if dist <= at && !self.done.contains(&(next, stage)) {
                    due = Some((stage, at));
                }
            }
        }
        if let Some((stage, _)) = due {
            for (s, _) in stages {
                if s <= stage {
                    self.done.insert((next, s));
                }
            }
            // A stage that couldn't be finished before the next one is due is dropped too.
            if stage != Stage::Now {
                let next_at = stages
                    .iter()
                    .filter(|(s, at)| *s > stage && at.is_some())
                    .map(|(_, at)| at.unwrap_or(0.0))
                    .fold(0.0, f64::max);
                if dist - next_at < speed.max(MIN_TIMING_SPEED) * SPEAK_SECS {
                    return None;
                }
            }
            let alert = m.verbal_alert.clone().unwrap_or_else(|| m.instruction.clone());
            let text = match stage {
                // Arrival is announced when it happens (see the session).
                Stage::Now if is_arrival(m.kind) => return None,
                Stage::Now => m.verbal_pre.clone().unwrap_or(alert),
                _ => with_distance(dist, &alert),
            };
            return Some(Prompt { text, maneuver: next, stage });
        }
        // Just past a maneuver onto a long road: "Continue for 2 miles." (Not after the start:
        // the opening prompt covers it.)
        if changed && next > 0 && !is_start(route.maneuvers[next - 1].kind) && !self.done.contains(&(next - 1, Stage::Post)) {
            self.done.insert((next - 1, Stage::Post));
            let far = stages[0].1.unwrap_or(FAR_SURFACE_M);
            if dist > far + POST_MARGIN_M {
                if let Some(text) = route.maneuvers[next - 1].verbal_post.clone() {
                    return Some(Prompt { text, maneuver: next - 1, stage: Stage::Post });
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::route::testing::route_through;
    use super::super::route::Mode;
    use super::*;

    fn north(m: f64) -> (f64, f64) {
        (39.0 + m / 111_195.0, -105.0)
    }

    #[test]
    fn distances_are_spoken_in_miles_and_feet() {
        assert_eq!(spoken_distance(152.4), "500 feet");
        assert_eq!(spoken_distance(90.0), "300 feet");
        assert_eq!(spoken_distance(400.0), "a quarter mile");
        assert_eq!(spoken_distance(805.0), "a half mile");
        assert_eq!(spoken_distance(1600.0), "1 mile");
        assert_eq!(spoken_distance(4828.0), "3 miles");
        assert_eq!(spoken_distance(2414.0), "1.5 miles");
    }

    /// Drive the route at `speed` m/s, one update a second; (distance before the maneuver,
    /// stage) for every prompt about maneuver 1.
    fn drive(route: &NavRoute, speed: f64) -> Vec<(f64, Stage, String)> {
        let mut p = Prompter::default();
        let mut out = Vec::new();
        let mut along = 0.0;
        while along < route.length_m() {
            if let Some(pr) = p.update(route, along, speed) {
                if pr.maneuver == 1 {
                    out.push((route.maneuver_at[1] - along, pr.stage, pr.text));
                }
            }
            along += speed;
        }
        out
    }

    #[test]
    fn surface_street_prompts_at_half_a_mile_500_feet_and_the_turn() {
        let r = route_through(&[north(0.0), north(3000.0), (39.0 + 3000.0 / 111_195.0, -104.99)], Mode::Avoid);
        let said = drive(&r, 13.0);
        let stages: Vec<Stage> = said.iter().map(|s| s.1).collect();
        assert_eq!(stages, vec![Stage::Far, Stage::Near, Stage::Now], "{said:?}");
        assert!(said[0].0 <= FAR_SURFACE_M && said[0].0 > FAR_SURFACE_M - 14.0);
        assert_eq!(said[0].2, "In a half mile, Turn 1.");
        assert!(said[1].0 <= NEAR_M && said[1].0 > NEAR_M - 14.0);
        assert_eq!(said[1].2, "In 500 feet, Turn 1.");
        assert!(said[2].0 <= 13.0 * NOW_SECS + 1.0);
        assert_eq!(said[2].2, "Turn 1 now.");
    }

    #[test]
    fn highway_prompts_start_at_a_mile_and_skip_500_feet_at_speed() {
        let mut r = route_through(&[north(0.0), north(5000.0), (39.0 + 5000.0 / 111_195.0, -104.99)], Mode::Avoid);
        r.maneuvers[0].highway = true;
        let said = drive(&r, 30.0);
        let stages: Vec<Stage> = said.iter().map(|s| s.1).collect();
        assert_eq!(stages, vec![Stage::Far, Stage::Now], "{said:?}");
        assert!(said[0].0 <= FAR_HIGHWAY_M && said[0].0 > FAR_HIGHWAY_M - 31.0);
        assert!(said[0].2.starts_with("In 1 mile, "));
    }

    #[test]
    fn a_turn_right_after_the_last_skips_the_stages_already_passed() {
        // Maneuver 1 only 300 m after the start: no half-mile prompt, straight to 500 ft.
        let r = route_through(&[north(0.0), north(300.0), (39.0 + 300.0 / 111_195.0, -104.99)], Mode::Avoid);
        let said = drive(&r, 13.0);
        let stages: Vec<Stage> = said.iter().map(|s| s.1).collect();
        assert_eq!(stages, vec![Stage::Far, Stage::Near, Stage::Now], "{said:?}");
        // The "far" prompt is said at once with the real distance.
        assert_eq!(said[0].2, "In 1000 feet, Turn 1.");
    }

    #[test]
    fn a_jump_past_several_stages_says_only_the_latest() {
        let r = route_through(&[north(0.0), north(3000.0), (39.0 + 3000.0 / 111_195.0, -104.99)], Mode::Avoid);
        let mut p = Prompter::default();
        assert!(p.update(&r, 100.0, 13.0).is_none());
        // Signal back after a gap, 30 m before the turn: only "now".
        let pr = p.update(&r, r.maneuver_at[1] - 30.0, 13.0).unwrap();
        assert_eq!(pr.stage, Stage::Now);
        // Standing still there: nothing is repeated.
        for _ in 0..30 {
            assert!(p.update(&r, r.maneuver_at[1] - 30.0, 0.0).is_none());
        }
    }

    #[test]
    fn continue_for_is_said_after_a_turn_onto_a_long_road() {
        let mut r = route_through(&[north(0.0), north(200.0), (39.0 + 200.0 / 111_195.0, -104.95)], Mode::Avoid);
        r.maneuvers[1].verbal_post = Some("Continue for 3 miles.".into());
        let mut p = Prompter::default();
        let mut texts = Vec::new();
        let mut along = 0.0;
        while along < 400.0 {
            if let Some(pr) = p.update(&r, along, 13.0) {
                texts.push(pr.text);
            }
            along += 13.0;
        }
        assert!(texts.contains(&"Continue for 3 miles.".to_string()), "{texts:?}");
    }
}
