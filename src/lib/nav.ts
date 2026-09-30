// Turn-by-turn navigation: types, commands and formatting for the navigation screen.
// The session itself runs in Rust (src-tauri/src/nav/); this side only shows it.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { formatRouteDistance, type RouteChoice } from "./directions";
import type { Maneuver, PlannedCamera, PlannedRoute } from "./types";

export type NavMode = "avoid" | "fastest";
export type NavState = "navigating" | "off_route" | "rerouting" | "lost_signal" | "paused" | "arrived";

export interface NavDestination {
  lat: number;
  lon: number;
  label: string;
}

export interface StepView {
  index: number;
  kind: number;
  /** Maneuver icon name (src/lib/maneuverIcons.json). */
  icon: string;
  instruction: string;
  street: string | null;
  distance_m: number;
  exit_number: string | null;
  roundabout_exit_count: number | null;
  bearing_before: number | null;
  bearing_after: number | null;
}

export interface AlertCamera {
  key: string;
  lat: number;
  lon: number;
  source: "osm" | "submission";
  category: string;
  operator: string | null;
  distance_m: number;
}

export interface CameraAlert {
  stage: "far" | "near";
  cameras: AlertCamera[];
  text: string;
}

export interface NavSession {
  state: NavState;
  mode: NavMode;
  destination: NavDestination;
  route_version: number;
  position: {
    lat: number;
    lon: number;
    accuracy_m: number;
    /** Matched to the route, while on it. */
    snapped: [number, number] | null;
    bearing: number;
    speed_mps: number;
  } | null;
  progress_m: number;
  remaining_m: number;
  remaining_s: number;
  eta_ms: number | null;
  step: StepView | null;
  /** The maneuver right after `step`, when it follows within 150 m. */
  then: StepView | null;
  cameras_ahead: number;
  cameras_passed: number;
  camera_alert: CameraAlert | null;
  camera_alert_ms: number | null;
  weak_signal: boolean;
  acquiring: boolean;
  notice: string | null;
  camera_change: { before: number; after: number } | null;
  summary: { elapsed_s: number; distance_m: number; cameras_passed: number } | null;
  muted: boolean;
  now_ms: number;
  started_ms: number;
  simulated: boolean;
  voice_unavailable: boolean;
  throttled: boolean;
}

export interface NavRouteView {
  version: number;
  mode: NavMode;
  shape: [number, number][];
  cameras: PlannedCamera[];
  maneuvers: Maneuver[];
}

export interface ActiveTrip {
  destination: NavDestination;
  mode: NavMode;
  started_ms: number;
}

export interface NavStatus {
  session: NavSession | null;
  resume: ActiveTrip | null;
  ended: string | null;
}

export interface Readiness {
  platform: "android" | "desktop";
  device_location: boolean;
  precise: boolean;
  approximate: boolean;
  denied_permanently: boolean;
  location_enabled: boolean;
  notifications: boolean;
  play_services: boolean;
  power_save_gps_off: boolean;
}

export interface SimParams {
  speed_mps: number;
  noise_m: number;
  rate: number;
  seed: number;
}

export const DEFAULT_SIM: SimParams = { speed_mps: 13.4, noise_m: 4, rate: 1, seed: 7 };

export type SimCommand =
  | { type: "deviate"; meters: number }
  | { type: "lose_signal"; secs: number }
  | { type: "speed"; mps: number }
  | { type: "noise"; meters: number }
  | { type: "rate"; rate: number };

export const navApi = {
  /** `null` when a GPX pick for a replay was cancelled. */
  start: (mode: NavMode, route: PlannedRoute, destination: NavDestination, simulate?: SimParams, replayGpx = false) =>
    invoke<NavSession | null>("nav_start", { args: { mode, route, destination, simulate: simulate ?? null, replay_gpx: replayGpx } }),
  stop: () => invoke<void>("nav_stop"),
  status: () => invoke<NavStatus>("nav_status"),
  route: () => invoke<NavRouteView | null>("nav_route"),
  coveredCameras: () => invoke<string[]>("nav_covered_cameras"),
  setMuted: (muted: boolean) => invoke<void>("nav_set_muted", { muted }),
  sim: (command: SimCommand) => invoke<void>("nav_sim", { command }),
  forgetResume: () => invoke<void>("nav_forget_resume"),
  readiness: () => invoke<Readiness>("nav_readiness"),
  request: (what: "request_location" | "enable_location" | "request_notifications" | "open_app_settings") =>
    invoke<Readiness>("nav_request", { what }),
  keepScreenOn: (on: boolean) => invoke<void>("nav_keep_screen_on", { on }),
};

export function onNavState(handler: (s: NavSession) => void): Promise<UnlistenFn> {
  return listen<NavSession>("nav:state", (e) => handler(e.payload));
}

export function onNavRoute(handler: (r: NavRouteView) => void): Promise<UnlistenFn> {
  return listen<NavRouteView>("nav:route", (e) => handler(e.payload));
}

export function onNavEnded(handler: (reason: string | null) => void): Promise<UnlistenFn> {
  return listen<{ reason: string | null }>("nav:ended", (e) => handler(e.payload.reason));
}

/** Keys of the cameras the navigation session alerts for; the map's proximity alerts skip them so
 * the same camera is never announced twice. */
export const navCovered = new Set<string>();

export const navModeFor = (choice: RouteChoice): NavMode => choice;

/** Navigation distances are in miles and feet (feet under 0.1 mi). */
export function navDistance(m: number): string {
  return formatRouteDistance(m, true);
}

/** "12 min", "1 h 5 min" */
export function navDuration(s: number): string {
  const min = Math.max(0, Math.round(s / 60));
  if (min < 60) return `${min} min`;
  return `${Math.floor(min / 60)} h ${min % 60} min`;
}

/** Arrival time as a clock time ("5:42 PM"). */
export function navClock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** Above this speed, the destination can't be typed (the driver shouldn't be typing). */
export const TYPING_LOCK_MPS = 2.2352; // 5 mph

export const STATE_LABEL: Partial<Record<NavState, string>> = {
  off_route: "Off route",
  rerouting: "Rerouting…",
  lost_signal: "GPS signal lost — estimating your position",
  paused: "Waiting for GPS signal — guidance paused",
};

/** What to show under the maneuver: its exit, and the road it goes onto when the instruction
 * doesn't already name it. */
export function stepDetail(s: StepView): string | null {
  const named = s.street && s.street.split(" / ").some((n) => s.instruction.includes(n));
  const street = named ? null : s.street;
  if (s.exit_number) return street ? `Exit ${s.exit_number} · ${street}` : `Exit ${s.exit_number}`;
  return street;
}

/** Why the Start button can't start yet, and what the user can do about it. */
export type Blocker =
  | { kind: "desktop" }
  | { kind: "permission" }
  | { kind: "approximate" }
  | { kind: "denied" }
  | { kind: "location_off" };

export function blockerFor(r: Readiness, simulated: boolean): Blocker | null {
  if (!r.device_location) return simulated ? null : { kind: "desktop" };
  if (!r.precise && !r.approximate) return r.denied_permanently ? { kind: "denied" } : { kind: "permission" };
  if (!r.precise && !simulated) return { kind: "approximate" };
  if (!r.location_enabled && !simulated) return { kind: "location_off" };
  return null;
}

export const BLOCKER_TEXT: Record<Blocker["kind"], string> = {
  desktop: "Turn-by-turn navigation uses the phone's location, so it runs on Android. On the desktop only the simulator can drive it (debug builds).",
  permission: "Navigation needs your precise location to follow you along the route.",
  approximate:
    "Flock Finder only has your approximate location. Turn-by-turn navigation needs precise location to tell which road you're on. Allow \"Precise\" for Flock Finder in its location permission.",
  denied: "Location permission is turned off for Flock Finder. Allow precise location in the app's settings to navigate.",
  location_off: "Location services are turned off. Turn them on to navigate.",
};
