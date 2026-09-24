import { haversineM } from "./geo";
import type { Camera } from "./types";

/** Live position from the locate button. Held in memory only; never persisted or uploaded. */
export interface UserPosition {
  lat: number;
  lon: number;
  /** 95% confidence radius in metres, as reported by the Geolocation API. */
  accuracy: number;
  at: number;
}

export interface ProximitySettings {
  enabled: boolean;
  radiusM: number;
  sound: boolean;
}

export const DEFAULT_PROXIMITY: ProximitySettings = { enabled: true, radiusM: 200, sound: true };
export const RADIUS_CHOICES = [100, 200, 300, 500, 1000];
/** Fixes vaguer than this (coarse Wi-Fi/cell positioning) never trigger an alert. */
export const MAX_ACCURACY_M = 500;
/** A camera can alert again only after this long… */
export const REALERT_MS = 15 * 60 * 1000;
/** …and only after you have been this many radii away from it (so edge jitter can't re-trigger). */
export const EXIT_FACTOR = 1.5;

export interface ProximityTarget {
  key: string;
  lat: number;
  lon: number;
}

export interface ProximityHit {
  key: string;
  distanceM: number;
}

/** The alert currently shown in the banner. */
export interface ProximityAlert {
  camera: Camera;
  distanceM: number;
  /** Bearing from you to the camera, degrees clockwise from north. */
  bearing: number;
  /** Other cameras that entered range with this one. */
  more: number;
  at: number;
}

/** Per-session memory of which cameras are in range and when each last alerted. */
export interface ProximityTracker {
  inside: Set<string>;
  lastAlert: Map<string, number>;
}

export const newTracker = (): ProximityTracker => ({ inside: new Set(), lastAlert: new Map() });

/**
 * Cameras that have just come within `radiusM` of `pos`, nearest first. A camera alerts once
 * on entering its radius; it has to drop out beyond EXIT_FACTOR × radius and REALERT_MS has to
 * pass before it can alert again.
 */
export function checkProximity(
  pos: UserPosition,
  targets: ProximityTarget[],
  radiusM: number,
  tracker: ProximityTracker,
  now: number,
): ProximityHit[] {
  if (pos.accuracy > MAX_ACCURACY_M) return [];
  const exitM = radiusM * EXIT_FACTOR;
  const hits: ProximityHit[] = [];
  for (const t of targets) {
    const d = haversineM(pos.lat, pos.lon, t.lat, t.lon);
    if (tracker.inside.has(t.key)) {
      if (d > exitM) tracker.inside.delete(t.key);
      continue;
    }
    if (d > radiusM) continue;
    tracker.inside.add(t.key);
    const last = tracker.lastAlert.get(t.key);
    if (last !== undefined && now - last < REALERT_MS) continue;
    tracker.lastAlert.set(t.key, now);
    hits.push({ key: t.key, distanceM: d });
  }
  return hits.sort((a, b) => a.distanceM - b.distanceM);
}

/** Initial compass bearing from point 1 to point 2, degrees clockwise from north in [0, 360). */
export function bearingDeg(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const toRad = (d: number) => (d * Math.PI) / 180;
  const φ1 = toRad(lat1);
  const φ2 = toRad(lat2);
  const Δλ = toRad(lon2 - lon1);
  const y = Math.sin(Δλ) * Math.cos(φ2);
  const x = Math.cos(φ1) * Math.sin(φ2) - Math.sin(φ1) * Math.cos(φ2) * Math.cos(Δλ);
  return ((Math.atan2(y, x) * 180) / Math.PI + 360) % 360;
}

const STORAGE_KEY = "flockfinder.proximity";

/** Per-device preferences; any missing or invalid value falls back to the default. */
export function loadProximitySettings(): ProximitySettings {
  try {
    const raw = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<ProximitySettings>;
    return {
      enabled: typeof raw.enabled === "boolean" ? raw.enabled : DEFAULT_PROXIMITY.enabled,
      radiusM: RADIUS_CHOICES.includes(Number(raw.radiusM)) ? Number(raw.radiusM) : DEFAULT_PROXIMITY.radiusM,
      sound: typeof raw.sound === "boolean" ? raw.sound : DEFAULT_PROXIMITY.sound,
    };
  } catch {
    return { ...DEFAULT_PROXIMITY };
  }
}

export function saveProximitySettings(p: ProximitySettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    /* storage unavailable: the choice lasts for this session only */
  }
}
