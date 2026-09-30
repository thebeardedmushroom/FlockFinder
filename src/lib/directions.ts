// Directions: formatting, messages and map features for a camera-avoiding route plan.
// The planning itself happens in Rust (src-tauri/src/routing.rs).
import type * as GeoJSON from "geojson";
import { errorMessage, isAppError, type PlannedRoute, type RemainingReason, type RoutePlan } from "./types";

export type RouteChoice = "avoid" | "fastest";

/** A start or destination: typed and geocoded, picked on the map, or the device position. */
export interface Endpoint {
  lat: number;
  lon: number;
  label: string;
}

/** Settings → Directions: the detour limit choices, minutes per camera avoided (0: no limit). */
export const DETOUR_LIMIT_CHOICES = [0, 2, 5, 10, 15];

/** Mirrors `AVOID_RADIUS_M` in routing.rs. */
export const AVOID_RADIUS_M = 30;

/** Road distances in miles where road signs use them. */
export function usesMiles(locale: string = typeof navigator === "undefined" ? "en-US" : navigator.language): boolean {
  return /^en-(US|GB|LR)\b/i.test(locale) || /^my\b/i.test(locale);
}

export function formatRouteDistance(m: number, miles = usesMiles()): string {
  if (miles) {
    const mi = m / 1609.344;
    if (mi < 0.1) return `${Math.max(50, Math.round((m * 3.28084) / 50) * 50)} ft`;
    return `${mi.toFixed(mi < 100 ? 1 : 0)} mi`;
  }
  if (m < 1000) return `${Math.max(10, Math.round(m / 10) * 10)} m`;
  return `${(m / 1000).toFixed(m < 100_000 ? 1 : 0)} km`;
}

export function formatDuration(s: number): string {
  const min = Math.max(1, Math.round(s / 60));
  if (min < 60) return `${min} min`;
  const h = Math.floor(min / 60);
  const rest = min % 60;
  return rest ? `${h} h ${rest} min` : `${h} h`;
}

/** "+4 min vs fastest" (whole minutes; under a minute reads as the same time). */
export function formatDelta(avoidS: number, fastestS: number): string {
  const d = Math.round((avoidS - fastestS) / 60);
  if (d === 0) return "same time as fastest";
  return `${d > 0 ? "+" : "−"}${formatDuration(Math.abs(d) * 60)} vs fastest`;
}

export function stretchCount(n: number): string {
  return `${n} stretch${n === 1 ? "" : "es"}`;
}

export function cameraCount(n: number): string {
  return `${n} camera${n === 1 ? "" : "s"}`;
}

/** "Avoidance: 14.2 mi, 26 min, 0 cameras (+4 min vs fastest)" */
export function summaryLine(choice: RouteChoice, plan: RoutePlan, miles = usesMiles()): string {
  const r = plan[choice];
  const base = `${choice === "avoid" ? "Avoidance" : "Fastest"}: ${formatRouteDistance(r.distance_m, miles)}, ${formatDuration(r.duration_s)}, ${cameraCount(r.cameras.length)}`;
  if (choice === "avoid" && !plan.same_route) return `${base} (${formatDelta(r.duration_s, plan.fastest.duration_s)})`;
  return base;
}

export const REMAINING_TEXT: Record<RemainingReason, string> = {
  near_endpoint: "At your start or destination, so it can't be avoided",
  unavoidable: "No way around it: the road map has no camera-free route past it",
  long_detour: "Going around it would take longer than your detour limit (Settings → Directions), so the route stays on this road",
  nearby: `The route still passes within ${AVOID_RADIUS_M} m of it on another road`,
  no_route: "Routing around it left the routing server with no route",
  search_limit: "The search hit its limits before avoiding this camera; a way around may exist",
};

export interface Headline {
  tone: "success" | "info" | "warn";
  text: string;
}

/** What the plan means, in one or two sentences, most important first. */
export function planHeadlines(plan: RoutePlan): Headline[] {
  const out: Headline[] = [];
  const a = plan.avoid.cameras.length;
  const f = plan.fastest.cameras.length;
  if (plan.outcome === "clear") {
    out.push(
      plan.same_route
        ? { tone: "success", text: "The fastest route already passes no mapped cameras." }
        : {
            tone: "success",
            text:
              `Camera-free route found, avoiding all ${cameraCount(f)} on the fastest route.` +
              (plan.road_check.status === "stretches"
                ? ` It's a long trip, so the ${stretchCount(plan.road_check.total)} of it with cameras ${plan.road_check.total === 1 ? "was" : "were"} rerouted one at a time.`
                : plan.avoid_from_road_map
                  ? " It was found by checking the road map around your trip."
                  : ""),
          },
    );
  } else {
    // Say what was actually established: only the road map can show that no way around exists.
    const best = plan.outcome === "unchanged" ? `Both routes pass ${cameraCount(a)}` : `The avoidance route passes the fewest found: ${cameraCount(a)} (fastest: ${f})`;
    const listed = "They're listed below and ringed on the map.";
    const allAtEnds = plan.avoid.cameras.every((c) => c.remaining === "near_endpoint");
    const check = plan.road_check;
    let text: string;
    if (allAtEnds) {
      text = `The only cameras left are at your start or destination, so no route can avoid them. ${best}. ${listed}`;
    } else if (check.status === "none_exists") {
      text = `No camera-free route exists: the road map around your trip has no way through without passing a camera. ${best}. ${listed}`;
    } else if (check.status === "camera_free") {
      text = `The road map shows a camera-free route, but the routing server couldn't be kept on it. ${best}. ${listed}`;
    } else if (check.status === "stretches") {
      const blocked = plan.avoid.cameras.filter((c) => c.remaining === "unavoidable").length;
      const slow = plan.avoid.cameras.filter((c) => c.remaining === "long_detour").length;
      const rest = a - blocked - slow - plan.avoid.cameras.filter((c) => c.remaining === "near_endpoint").length;
      text =
        `This long trip was checked stretch by stretch: ${check.fixed} of the ${stretchCount(check.total)} with cameras ${check.fixed === 1 ? "was" : "were"} rerouted around them. ` +
        (blocked > 0 ? `The road map shows no way around ${cameraCount(blocked)}. ` : "") +
        (slow > 0
          ? `Going around ${cameraCount(slow)} would add more than ${check.limit_min ?? 0} minutes each (your detour limit in Settings), so the route keeps ${slow === 1 ? "that road" : "those roads"}. `
          : "") +
        (rest > 0 ? `For ${blocked + slow > 0 ? "the other " : ""}${cameraCount(rest)}, the search stopped before finding a way around, so one may exist. ` : "") +
        `${best}. ${listed}`;
    } else if (check.status === "unavailable") {
      text = `No camera-free route was found, but one may exist: ${check.reason}. ${best}. ${listed}`;
    } else {
      text = `No camera-free route was found before the search stopped, so one may exist. ${best}. ${listed}`;
    }
    out.push({ tone: "warn", text });
    // Never let a limit pass silently.
    const hit = [plan.limits.exclusion_cap && "50 excluded cameras per request", plan.limits.request_budget && "8 requests"].filter(Boolean);
    if (hit.length && check.status !== "none_exists") {
      out.push({ tone: "info", text: `The first search reached the routing server's limits (${hit.join(" and ")}).` });
    }
  }
  if (plan.long_detour) {
    const extra = plan.avoid.duration_s - plan.fastest.duration_s;
    const pct = plan.fastest.duration_s > 0 ? Math.round((extra / plan.fastest.duration_s) * 100) : 0;
    out.push({
      tone: "warn",
      text: `Long detour: the avoidance route takes ${formatDuration(extra)} longer (+${pct}%) than the fastest route.`,
    });
  }
  if (plan.warning) out.push({ tone: "warn", text: plan.warning });
  return out;
}

/** What the road-map check loaded, when it ran. */
export function roadMapNote(plan: RoutePlan): string | null {
  const m = plan.road_map;
  if (!m) return null;
  const mb = `${(m.downloaded_bytes / 1e6).toFixed(1)} MB of map data`;
  const roads = `${m.ways.toLocaleString("en-US")} roads`;
  if (m.downloaded_tiles === m.tiles) return `Checked the road map around your trip: ${roads}, downloaded now (${mb}).`;
  if (m.cached_tiles === m.tiles) return `Checked the road map around your trip: ${roads}, all from the saved copy.`;
  const parts = [
    m.downloaded_tiles > 0 && `${m.downloaded_tiles} downloaded now (${mb})`,
    m.cached_tiles > 0 && `${m.cached_tiles} from the saved copy`,
    m.missing_tiles > 0 && `${m.missing_tiles} not downloaded in time, so a later trip here will fetch them`,
  ].filter(Boolean);
  return `Checked the road map around your trip (${m.tiles} areas, ${roads}): ${parts.join("; ")}.`;
}

/** A readable message for a failed plan; the map keeps whatever it showed. */
export function routingErrorMessage(e: unknown, server = "the routing server"): string {
  if (isAppError(e)) {
    switch (e.kind) {
      case "offline":
        return `Can't reach ${server}. Check your connection and try again.`;
      case "rate_limited":
        return `${server} is busy or limiting requests right now. Wait a minute and try again.`;
      case "invalid":
        return e.message.replace(/^invalid input:\s*/i, "");
      case "http":
        return `${server} returned an error: ${e.message.replace(/^HTTP \d+ from [^:]+:\s*/, "")}`;
      default:
        return `Routing failed: ${e.message}`;
    }
  }
  return `Routing failed: ${errorMessage(e)}`;
}

function lineFeature(r: PlannedRoute, kind: RouteChoice, selected: boolean): GeoJSON.Feature {
  return {
    type: "Feature",
    geometry: { type: "LineString", coordinates: r.shape.map(([lat, lon]) => [lon, lat]) },
    properties: { kind, selected },
  };
}

/** Both routes, the selected one last (drawn on top). One line when they are the same route. */
export function routeFeatures(plan: RoutePlan, selected: RouteChoice): GeoJSON.Feature[] {
  if (plan.same_route) return [lineFeature(plan.avoid, "avoid", true)];
  const other: RouteChoice = selected === "avoid" ? "fastest" : "avoid";
  return [lineFeature(plan[other], other, false), lineFeature(plan[selected], selected, true)];
}

export function routeCameraFeatures(plan: RoutePlan, selected: RouteChoice): GeoJSON.Feature[] {
  return plan[selected].cameras.map((c) => ({
    type: "Feature",
    geometry: { type: "Point", coordinates: [c.lon, c.lat] },
    properties: { key: c.key },
  }));
}

export function endpointFeatures(start: Endpoint | null, end: Endpoint | null): GeoJSON.Feature[] {
  const out: GeoJSON.Feature[] = [];
  if (start) out.push({ type: "Feature", geometry: { type: "Point", coordinates: [start.lon, start.lat] }, properties: { role: "start", label: "A" } });
  if (end) out.push({ type: "Feature", geometry: { type: "Point", coordinates: [end.lon, end.lat] }, properties: { role: "end", label: "B" } });
  return out;
}

export function isAndroid(): boolean {
  return typeof navigator !== "undefined" && /Android/i.test(navigator.userAgent);
}

export function coordLabel(lat: number, lon: number): string {
  return `${lat.toFixed(5)}, ${lon.toFixed(5)}`;
}
