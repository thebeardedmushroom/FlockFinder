import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_PROXIMITY,
  MAX_ACCURACY_M,
  REALERT_MS,
  bearingDeg,
  checkProximity,
  loadProximitySettings,
  newTracker,
  saveProximitySettings,
  type UserPosition,
} from "../lib/proximity";

// 0.001° of latitude ≈ 111 m.
const BASE = { lat: 39.75, lon: -105.0 };
const at = (dLat: number, accuracy = 20): UserPosition => ({ lat: BASE.lat + dLat, lon: BASE.lon, accuracy, at: 0 });
const cams = [
  { key: "node/1", lat: BASE.lat + 0.001, lon: BASE.lon }, // ~111 m north
  { key: "node/2", lat: BASE.lat + 0.0015, lon: BASE.lon }, // ~167 m north
  { key: "node/3", lat: BASE.lat + 0.01, lon: BASE.lon }, // ~1.1 km north
];

describe("checkProximity", () => {
  it("alerts once for each camera entering the radius, nearest first", () => {
    const tr = newTracker();
    const hits = checkProximity(at(0), cams, 200, tr, 0);
    expect(hits.map((h) => h.key)).toEqual(["node/1", "node/2"]);
    expect(hits[0].distanceM).toBeGreaterThan(100);
    expect(hits[0].distanceM).toBeLessThan(120);
    expect(checkProximity(at(0), cams, 200, tr, 1000)).toEqual([]);
  });

  it("does not re-alert for jitter just outside the radius", () => {
    const tr = newTracker();
    checkProximity(at(0), cams, 200, tr, 0);
    // ~278 m / ~222 m away: outside the 200 m radius but inside the 300 m exit band.
    expect(checkProximity(at(0.0035), cams, 200, tr, 1000)).toEqual([]);
    expect(checkProximity(at(0), cams, 200, tr, 2000)).toEqual([]);
  });

  it("re-alerts only after leaving the exit band and waiting out the cooldown", () => {
    const tr = newTracker();
    checkProximity(at(0), cams, 200, tr, 0);
    checkProximity(at(0.006), cams, 200, tr, 60_000); // ~555 m / ~500 m: both have left
    expect(checkProximity(at(0), cams, 200, tr, 120_000)).toEqual([]); // within cooldown
    checkProximity(at(0.006), cams, 200, tr, 130_000);
    const again = checkProximity(at(0), cams, 200, tr, REALERT_MS + 200_000);
    expect(again.map((h) => h.key)).toEqual(["node/1", "node/2"]);
  });

  it("ignores fixes too vague to trust", () => {
    const tr = newTracker();
    expect(checkProximity(at(0, MAX_ACCURACY_M + 1), cams, 200, tr, 0)).toEqual([]);
    expect(tr.inside.size).toBe(0);
    expect(checkProximity(at(0, 30), cams, 200, tr, 1)).toHaveLength(2);
  });
});

describe("bearingDeg", () => {
  it("points north, east, south and west", () => {
    expect(bearingDeg(0, 0, 1, 0)).toBeCloseTo(0, 5);
    expect(bearingDeg(0, 0, 0, 1)).toBeCloseTo(90, 5);
    expect(bearingDeg(1, 0, 0, 0)).toBeCloseTo(180, 5);
    expect(bearingDeg(0, 1, 0, 0)).toBeCloseTo(270, 5);
  });
});

describe("proximity settings storage", () => {
  beforeEach(() => localStorage.clear());

  it("round-trips and falls back to defaults for missing or invalid values", () => {
    expect(loadProximitySettings()).toEqual(DEFAULT_PROXIMITY);
    saveProximitySettings({ enabled: false, radiusM: 500, sound: false });
    expect(loadProximitySettings()).toEqual({ enabled: false, radiusM: 500, sound: false });
    localStorage.setItem("flockfinder.proximity", JSON.stringify({ enabled: "yes", radiusM: 123 }));
    expect(loadProximitySettings()).toEqual(DEFAULT_PROXIMITY);
    localStorage.setItem("flockfinder.proximity", "not json");
    expect(loadProximitySettings()).toEqual(DEFAULT_PROXIMITY);
  });
});
