import { describe, expect, it } from "vitest";
import {
  activeFilterCount,
  cameraVisible,
  filterReducer,
  initialFilters,
  sightingVisible,
  submissionVisible,
  type FilterState,
} from "../lib/filters";
import type { Camera, Submission, WifiSighting } from "../lib/types";

const cam = (over: Partial<Camera> = {}): Camera => ({
  osm_type: "node",
  osm_id: 1,
  lat: 0,
  lon: 0,
  category: "alpr",
  tags: {},
  first_seen: 0,
  last_seen: 0,
  stale_since: null,
  ...over,
});

const sub = (over: Partial<Submission> = {}): Submission => ({
  id: 1,
  lat: 0,
  lon: 0,
  category: "flock",
  direction: null,
  mount: null,
  operator: null,
  notes: null,
  status: "local",
  osm_element_id: null,
  created_at: 0,
  updated_at: 0,
  ...over,
});

const sighting = (): WifiSighting => ({
  netid: "70:C9:4E:00:00:01",
  lat: 0,
  lon: 0,
  oui: "70:C9:4E",
  ssid: null,
  channel: 6,
  encryption: null,
  first_seen: null,
  last_seen: null,
  city: null,
  region: null,
  country: null,
  road: null,
  postalcode: null,
  source: "upstream",
  imported_at: 0,
});

describe("filterReducer state machine", () => {
  it("starts with everything visible", () => {
    expect(initialFilters).toEqual({ flock: true, alpr: true, user: true, wifi: true, operator: "", cones: true });
    expect(activeFilterCount(initialFilters)).toBe(0);
  });

  it("toggles each category independently", () => {
    let s = filterReducer(initialFilters, { type: "toggle", kind: "flock" });
    expect(s).toMatchObject({ flock: false, alpr: true, user: true, wifi: true });
    s = filterReducer(s, { type: "toggle", kind: "user" });
    expect(s).toMatchObject({ flock: false, alpr: true, user: false, wifi: true });
    s = filterReducer(s, { type: "toggle", kind: "wifi" });
    expect(s).toMatchObject({ flock: false, alpr: true, user: false, wifi: false });
    s = filterReducer(s, { type: "toggle", kind: "flock" });
    expect(s).toMatchObject({ flock: true, alpr: true, user: false, wifi: false });
    expect(activeFilterCount(s)).toBe(2);
  });

  it("set is idempotent", () => {
    const s1 = filterReducer(initialFilters, { type: "set", kind: "alpr", value: false });
    const s2 = filterReducer(s1, { type: "set", kind: "alpr", value: false });
    expect(s2).toEqual(s1);
  });

  it("solo shows only one category and keeps the operator text", () => {
    const withText = filterReducer(initialFilters, { type: "operator", text: "police" });
    const s = filterReducer(withText, { type: "solo", kind: "user" });
    expect(s).toEqual({ flock: false, alpr: false, user: true, wifi: false, operator: "police", cones: true });
    expect(filterReducer(initialFilters, { type: "solo", kind: "wifi" })).toMatchObject({
      flock: false,
      alpr: false,
      user: false,
      wifi: true,
    });
  });

  it("reset returns to the initial state", () => {
    let s = filterReducer(initialFilters, { type: "solo", kind: "flock" });
    s = filterReducer(s, { type: "operator", text: "x" });
    expect(filterReducer(s, { type: "reset" })).toEqual(initialFilters);
  });

  it("treats the cones toggle as a display option, not an active filter", () => {
    const off = filterReducer(initialFilters, { type: "cones", value: false });
    expect(off.cones).toBe(false);
    expect(activeFilterCount(off)).toBe(0);
    expect(filterReducer(off, { type: "solo", kind: "flock" }).cones).toBe(false);
    expect(filterReducer(off, { type: "reset" }).cones).toBe(true);
  });

  it("does not mutate the previous state", () => {
    const before = { ...initialFilters };
    filterReducer(initialFilters, { type: "toggle", kind: "flock" });
    expect(initialFilters).toEqual(before);
  });
});

describe("visibility predicates", () => {
  it("hides categories that are toggled off, folding unknown into alpr", () => {
    const f: FilterState = { ...initialFilters, alpr: false };
    expect(cameraVisible(cam({ category: "flock" }), f)).toBe(true);
    expect(cameraVisible(cam({ category: "alpr" }), f)).toBe(false);
    expect(cameraVisible(cam({ category: "unknown" }), f)).toBe(false);
  });

  it("filters by operator substring, case-insensitively, on OSM cameras", () => {
    const f: FilterState = { ...initialFilters, operator: "denver" };
    expect(cameraVisible(cam({ tags: { operator: "Denver Police Department" } }), f)).toBe(true);
    expect(cameraVisible(cam({ tags: { operator: "Aurora PD" } }), f)).toBe(false);
    expect(cameraVisible(cam({ tags: {} }), f)).toBe(false);
    expect(cameraVisible(cam({ tags: {} }), { ...f, operator: "   " })).toBe(true);
  });

  it("applies the user toggle and operator text to local submissions", () => {
    expect(submissionVisible(sub(), initialFilters)).toBe(true);
    expect(submissionVisible(sub(), { ...initialFilters, user: false })).toBe(false);
    expect(submissionVisible(sub({ operator: "The Home Depot" }), { ...initialFilters, operator: "home" })).toBe(true);
    expect(submissionVisible(sub({ operator: null }), { ...initialFilters, operator: "home" })).toBe(false);
  });

  it("shows Wi-Fi sightings only when the layer is on and no operator text is set", () => {
    expect(sightingVisible(sighting(), initialFilters)).toBe(true);
    expect(sightingVisible(sighting(), { ...initialFilters, wifi: false })).toBe(false);
    expect(sightingVisible(sighting(), { ...initialFilters, operator: "police" })).toBe(false);
    expect(sightingVisible(sighting(), { ...initialFilters, operator: "  " })).toBe(true);
  });
});
