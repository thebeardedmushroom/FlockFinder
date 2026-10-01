// Directions planning shared by the Directions panel and quick navigation to a saved place.
import { useAppStore } from "../store/useAppStore";
import { isAndroid, routingErrorMessage, type Endpoint } from "./directions";
import { haversineM } from "./geo";
import { api } from "./ipc";
import { navApi, type Readiness } from "./nav";
import { ALREADY_HERE_M, LOCATION_TIMEOUT_MS } from "./places";
import type { SavedPlace } from "./types";

export const DEFAULT_SERVER = "valhalla1.openstreetmap.de";

/** Host of the configured routing server, for messages. */
export function serverName(endpoint: string | undefined): string {
  try {
    return endpoint ? new URL(endpoint).host : DEFAULT_SERVER;
  } catch {
    return endpoint || DEFAULT_SERVER;
  }
}

/** Quick navigation planned a route: the Directions panel scrolls to it. */
export const REVEAL_ROUTE_EVENT = "flockfinder:reveal-route";

/** Plans in flight; only the latest one's answer is shown. */
let planSeq = 0;

/** Forget any plan still running (its answer is dropped). */
export function cancelPlans(): void {
  planSeq++;
}

/**
 * Plan both routes (fastest and camera-avoiding, per the routing settings) and show them. A
 * failure is shown in the Directions panel and leaves the endpoints selected.
 */
export async function planDirections(s: Endpoint, e: Endpoint): Promise<void> {
  const seq = ++planSeq;
  const store = useAppStore.getState();
  store.setDirections({ busy: true, error: null, notice: null, progress: null });
  try {
    const result = await api.planRoute({ lat: s.lat, lon: s.lon }, { lat: e.lat, lon: e.lon });
    if (seq !== planSeq) return;
    store.setDirections({ plan: result, selected: "avoid", busy: false, progress: null });
    const b = [result.fastest.bbox, result.avoid.bbox];
    store.flyTo({
      bbox: {
        south: Math.min(b[0].south, b[1].south),
        west: Math.min(b[0].west, b[1].west),
        north: Math.max(b[0].north, b[1].north),
        east: Math.max(b[0].east, b[1].east),
      },
    });
  } catch (err) {
    if (seq !== planSeq) return;
    // The map keeps whatever route it was showing.
    // Named from the settings now, not at render time (they may have just changed).
    const host = serverName(useAppStore.getState().settings?.routing_endpoint);
    store.setDirections({ busy: false, progress: null, error: routingErrorMessage(err, host) });
  }
}

type Located = { ok: true; lat: number; lon: number } | { ok: false; reason: "denied" | "off" | "timeout" | "unavailable" };

/** The device position: a fresh one from the locate button, else a new fix (up to 15 s). */
function currentLocation(): Promise<Located> {
  const pos = useAppStore.getState().userPosition;
  if (pos && Date.now() - pos.at < 60_000) return Promise.resolve({ ok: true, lat: pos.lat, lon: pos.lon });
  if (typeof navigator === "undefined" || !("geolocation" in navigator)) return Promise.resolve({ ok: false, reason: "unavailable" });
  return new Promise((resolve) => {
    let settled = false;
    const finish = (r: Located) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timer);
      resolve(r);
    };
    // The API's own timeout only counts once permission is granted; this one counts from the tap.
    const timer = window.setTimeout(() => finish({ ok: false, reason: "timeout" }), LOCATION_TIMEOUT_MS);
    navigator.geolocation.getCurrentPosition(
      (p) => finish({ ok: true, lat: p.coords.latitude, lon: p.coords.longitude }),
      (err) => finish({ ok: false, reason: err.code === 1 ? "denied" : err.code === 3 ? "timeout" : "off" }),
      { enableHighAccuracy: true, timeout: LOCATION_TIMEOUT_MS, maximumAge: 30_000 },
    );
  });
}

/** Android: ask for location permission if it was never asked, and report what still blocks a fix. */
async function androidBlocker(): Promise<"denied" | "off" | null> {
  let r: Readiness;
  try {
    r = await navApi.readiness();
  } catch {
    return null; // let the WebView's geolocation report it
  }
  if (!r.device_location) return null;
  if (!r.precise && !r.approximate && !r.denied_permanently) {
    try {
      r = await navApi.request("request_location");
    } catch {
      /* fall through with what we know */
    }
  }
  if (!r.precise && !r.approximate) return "denied";
  if (!r.location_enabled) return "off";
  return null;
}

function openNavSettings(what: "open_app_settings" | "enable_location") {
  void navApi.request(what).catch(() => useAppStore.getState().pushToast("Could not open the settings.", "error"));
}

/** Android: why the current location isn't available, with the setting that fixes it. */
function showLocationProblem(reason: "denied" | "off" | "timeout" | "unavailable") {
  const store = useAppStore.getState();
  if (reason === "denied") {
    store.pushToast("Quick navigation starts from your current location. Allow location access for Flock Finder to use it.", "warn", {
      label: "Open settings",
      run: () => openNavSettings("open_app_settings"),
    });
  } else if (reason === "off") {
    store.pushToast("Location services are off. Quick navigation needs your current location to start from where you are.", "warn", {
      label: "Turn on location",
      run: () => openNavSettings("enable_location"),
    });
  } else if (reason === "timeout") {
    store.pushToast("Couldn't find your location within 15 seconds. Try again where the signal is better.", "error");
  } else {
    store.pushToast("Your current location isn't available on this device.", "error");
  }
}

let quickSeq = 0;

/**
 * Quick navigation: route from the current location to a saved place, then show the route
 * preview (with Start on Android). Resolves once the location step is done (the chip's
 * loading state); routing continues in the Directions panel.
 */
export async function navigateToPlace(place: SavedPlace): Promise<void> {
  const seq = ++quickSeq;
  const end: Endpoint = { lat: place.lat, lon: place.lon, label: place.label };
  const android = isAndroid();
  if (android) {
    const blocked = await androidBlocker();
    if (seq !== quickSeq) return;
    if (blocked) {
      showLocationProblem(blocked);
      return;
    }
  }
  const here = await currentLocation();
  if (seq !== quickSeq) return;
  const store = useAppStore.getState();
  if (!here.ok) {
    if (android) {
      showLocationProblem(here.reason);
      return;
    }
    // Desktop without a location source: choose the start in Directions instead.
    cancelPlans();
    store.setMode("view");
    store.setDirections({ start: null, end, plan: null, busy: false, progress: null, error: null, notice: null });
    store.setDirections({
      notice: `Your current location isn't available on this computer. Choose a start point (type an address or use Map), then Get route to ${place.label}.`,
    });
    store.setPanel("directions");
    return;
  }
  if (haversineM(here.lat, here.lon, place.lat, place.lon) < ALREADY_HERE_M) {
    store.pushToast(`You're already here (${place.label}).`, "info");
    return;
  }
  const start: Endpoint = { lat: here.lat, lon: here.lon, label: "My location" };
  store.setMode("view");
  store.select(null);
  store.setDirections({ start, end, plan: null, error: null, notice: null });
  store.setPanel("directions");
  await planDirections(start, end);
  // On a phone the result (and Start) is below the fields: bring it up.
  if (seq === quickSeq && useAppStore.getState().directions.plan) window.dispatchEvent(new CustomEvent(REVEAL_ROUTE_EVENT));
}
