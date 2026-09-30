import { describe, expect, it } from "vitest";
import {
  endpointFeatures,
  formatDelta,
  formatDuration,
  formatRouteDistance,
  planHeadlines,
  REMAINING_TEXT,
  roadMapNote,
  routeCameraFeatures,
  routeFeatures,
  routingErrorMessage,
  summaryLine,
  usesMiles,
} from "../lib/directions";
import type { PlannedRoute, PlannedCamera, RoutePlan } from "../lib/types";

const cam = (key: string): PlannedCamera => ({
  key,
  lat: 39,
  lon: -105,
  category: "flock",
  source: "osm",
  direction: null,
  operator: null,
  along_m: 100,
  distance_m: 5,
  remaining: null,
});

const route = (distance_m: number, duration_s: number, cameras: PlannedCamera[]): PlannedRoute => ({
  shape: [
    [39, -105],
    [39.1, -104.9],
  ],
  distance_m,
  duration_s,
  cameras,
  maneuvers: [],
  bbox: { south: 39, west: -105, north: 39.1, east: -104.9 },
});

const plan = (p: Partial<RoutePlan>): RoutePlan => ({
  fastest: route(20_000, 22 * 60, [cam("node/1"), cam("node/2")]),
  avoid: route(22_853, 26 * 60, []),
  same_route: false,
  outcome: "clear",
  long_detour: false,
  requests: 2,
  excluded: 2,
  warning: null,
  server: "valhalla1.openstreetmap.de",
  road_check: { status: "not_needed" },
  avoid_from_road_map: false,
  limits: { exclusion_cap: false, request_budget: false },
  road_map: null,
  ...p,
});

const leftover = (reason: PlannedCamera["remaining"]): PlannedCamera => ({ ...cam("node/3"), remaining: reason });

describe("directions formatting", () => {
  it("picks miles for US English and km elsewhere", () => {
    expect(usesMiles("en-US")).toBe(true);
    expect(usesMiles("en-GB")).toBe(true);
    expect(usesMiles("de-DE")).toBe(false);
    expect(usesMiles("en-CA")).toBe(false);
  });

  it("formats road distances", () => {
    expect(formatRouteDistance(22_853, true)).toBe("14.2 mi");
    expect(formatRouteDistance(40_000, true)).toBe("24.9 mi");
    expect(formatRouteDistance(200_000, true)).toBe("124 mi");
    expect(formatRouteDistance(60, true)).toBe("200 ft");
    expect(formatRouteDistance(850, false)).toBe("850 m");
    expect(formatRouteDistance(14_200, false)).toBe("14.2 km");
    expect(formatRouteDistance(4_260, false)).toBe("4.3 km");
  });

  it("formats durations and the difference from the fastest route", () => {
    expect(formatDuration(26 * 60)).toBe("26 min");
    expect(formatDuration(20)).toBe("1 min");
    expect(formatDuration(65 * 60)).toBe("1 h 5 min");
    expect(formatDelta(26 * 60, 22 * 60)).toBe("+4 min vs fastest");
    expect(formatDelta(22 * 60 + 20, 22 * 60)).toBe("same time as fastest");
  });

  it("writes the summary line from the spec", () => {
    expect(summaryLine("avoid", plan({}), true)).toBe("Avoidance: 14.2 mi, 26 min, 0 cameras (+4 min vs fastest)");
    expect(summaryLine("fastest", plan({}), true)).toBe("Fastest: 12.4 mi, 22 min, 2 cameras");
  });
});

describe("plan headlines", () => {
  it("says a camera-free route was found", () => {
    const h = planHeadlines(plan({}));
    expect(h).toHaveLength(1);
    expect(h[0].tone).toBe("success");
    expect(h[0].text).toContain("2 cameras");
  });

  it("says a camera-free route doesn't exist only when the road map shows it", () => {
    const h = planHeadlines(
      plan({ outcome: "reduced", road_check: { status: "none_exists", fewest: 1 }, avoid: route(21_000, 24 * 60, [leftover("unavoidable")]) }),
    );
    expect(h[0].tone).toBe("warn");
    expect(h[0].text).toMatch(/^No camera-free route exists: the road map/);
    expect(h[0].text).toContain("1 camera (fastest: 2)");
  });

  it("says a route may exist when the search stopped at its limits", () => {
    const h = planHeadlines(
      plan({
        outcome: "reduced",
        road_check: { status: "unavailable", reason: "the road map couldn't be downloaded because you're offline" },
        limits: { exclusion_cap: true, request_budget: true },
        avoid: route(21_000, 24 * 60, [leftover("search_limit")]),
      }),
    );
    expect(h[0].text).toMatch(/^No camera-free route was found, but one may exist: the road map couldn't be downloaded because you're offline\. The avoidance route/);
    expect(h[0].text).not.toMatch(/No camera-free route exists/);
    // The limits are said out loud, never swallowed.
    expect(h[1]).toEqual({ tone: "info", text: "The first search reached the routing server's limits (50 excluded cameras per request and 8 requests)." });
  });

  it("explains a long trip fixed stretch by stretch", () => {
    const clear = planHeadlines(plan({ road_check: { status: "stretches", fixed: 3, total: 3, too_slow: 0, limit_min: 5 }, avoid_from_road_map: true }));
    expect(clear[0].tone).toBe("success");
    expect(clear[0].text).toContain("long trip, so the 3 stretches of it with cameras were rerouted one at a time");
    const partial = planHeadlines(
      plan({
        outcome: "reduced",
        road_check: { status: "stretches", fixed: 2, total: 4, too_slow: 1, limit_min: 5 },
        avoid: route(22_853, 26 * 60, [
          leftover("unavoidable"),
          { ...leftover("long_detour"), key: "node/5" },
          { ...leftover("search_limit"), key: "node/4" },
        ]),
      }),
    );
    expect(partial[0].tone).toBe("warn");
    expect(partial[0].text).toContain("checked stretch by stretch: 2 of the 4 stretches with cameras were rerouted");
    expect(partial[0].text).toContain("The road map shows no way around 1 camera.");
    expect(partial[0].text).toContain("Going around 1 camera would add more than 5 minutes each (your detour limit in Settings), so the route keeps that road.");
    expect(partial[0].text).toContain("For the other 1 camera, the search stopped before finding a way around, so one may exist.");
    expect(partial[0].text).not.toContain("too long");
  });

  it("owns up when the road map has a way but the server strayed off it", () => {
    const h = planHeadlines(plan({ outcome: "reduced", road_check: { status: "camera_free" }, avoid: route(21_000, 24 * 60, [leftover("search_limit")]) }));
    expect(h[0].text).toMatch(/^The road map shows a camera-free route, but the routing server couldn't be kept on it/);
  });

  it("explains cameras left only at the start or destination", () => {
    const h = planHeadlines(plan({ outcome: "unchanged", fastest: route(1000, 60, [leftover("near_endpoint")]), avoid: route(1000, 60, [leftover("near_endpoint")]), same_route: true }));
    expect(h[0].text).toMatch(/^The only cameras left are at your start or destination/);
  });

  it("credits the road map for a camera-free route it found", () => {
    const h = planHeadlines(plan({ avoid_from_road_map: true, road_check: { status: "camera_free" } }));
    expect(h[0].tone).toBe("success");
    expect(h[0].text).toMatch(/found by checking the road map/);
  });

  it("notes what the road map check loaded", () => {
    expect(roadMapNote(plan({}))).toBeNull();
    const m = (downloaded_tiles: number, cached_tiles: number, missing_tiles: number) =>
      roadMapNote(plan({ road_map: { tiles: 15, downloaded_tiles, cached_tiles, missing_tiles, downloaded_bytes: 25_500_000, ways: 42_711 } }));
    expect(m(15, 0, 0)).toBe("Checked the road map around your trip: 42,711 roads, downloaded now (25.5 MB of map data).");
    expect(m(0, 15, 0)).toMatch(/all from the saved copy\.$/);
    // Never claim tiles came from the saved copy when they weren't downloaded at all.
    expect(m(6, 0, 9)).toBe(
      "Checked the road map around your trip (15 areas, 42,711 roads): 6 downloaded now (25.5 MB of map data); 9 not downloaded in time, so a later trip here will fetch them.",
    );
    expect(m(0, 12, 3)).toBe(
      "Checked the road map around your trip (15 areas, 42,711 roads): 12 from the saved copy; 3 not downloaded in time, so a later trip here will fetch them.",
    );
  });

  it("explains every leftover reason", () => {
    for (const r of ["near_endpoint", "unavoidable", "nearby", "no_route", "search_limit"] as const) expect(REMAINING_TEXT[r].length).toBeGreaterThan(10);
    expect(REMAINING_TEXT.search_limit).toMatch(/a way around may exist/);
  });

  it("flags a long detour and a partial search", () => {
    const h = planHeadlines(plan({ long_detour: true, avoid: route(40_000, 45 * 60, []), warning: "stopped early" }));
    expect(h.map((x) => x.text).join(" ")).toMatch(/Long detour: .*23 min longer \(\+105%\)/);
    expect(h[h.length - 1].text).toBe("stopped early");
  });

  it("handles a fastest route that is already clear", () => {
    const h = planHeadlines(plan({ same_route: true, fastest: route(1000, 60, []), avoid: route(1000, 60, []) }));
    expect(h[0].text).toMatch(/already passes no mapped cameras/);
  });
});

describe("map features", () => {
  it("draws the selected route last and one line for a shared route", () => {
    const f = routeFeatures(plan({}), "fastest");
    expect(f.map((x) => x.properties)).toEqual([
      { kind: "avoid", selected: false },
      { kind: "fastest", selected: true },
    ]);
    expect((f[0].geometry as { coordinates: number[][] }).coordinates[0]).toEqual([-105, 39]);
    expect(routeFeatures(plan({ same_route: true }), "fastest")).toHaveLength(1);
  });

  it("rings the cameras of the selected route and marks the endpoints", () => {
    expect(routeCameraFeatures(plan({}), "fastest")).toHaveLength(2);
    expect(routeCameraFeatures(plan({}), "avoid")).toHaveLength(0);
    const ends = endpointFeatures({ lat: 1, lon: 2, label: "a" }, null);
    expect(ends).toHaveLength(1);
    expect(ends[0].properties).toEqual({ role: "start", label: "A" });
  });
});

describe("routing errors", () => {
  it("turns backend errors into readable messages", () => {
    expect(routingErrorMessage({ kind: "offline", message: "network unavailable: x" }, "valhalla1.openstreetmap.de")).toBe(
      "Can't reach valhalla1.openstreetmap.de. Check your connection and try again.",
    );
    expect(routingErrorMessage({ kind: "rate_limited", message: "…" }, "srv")).toMatch(/^srv is busy/);
    expect(routingErrorMessage({ kind: "invalid", message: "invalid input: No driving route was found between these points." })).toBe(
      "No driving route was found between these points.",
    );
    expect(routingErrorMessage({ kind: "http", message: "HTTP 500 from the routing server: boom" }, "srv")).toBe("srv returned an error: boom");
    expect(routingErrorMessage(new Error("weird"))).toMatch(/^Routing failed/);
  });
});
