// The map while navigating: the car marker, a camera that follows it (heading up while moving,
// or north up), and the part of the route still ahead.
//
// MapLibre GL JS has no location component (its GeolocateControl reads the WebView's own
// Geolocation API and can't take positions from the navigation session), so the marker is an
// HTML marker rotated to the direction of travel, and following is done with easeTo. Moves the
// user makes (drag, pinch, rotate) end following; Recenter, or 10 s without touching the map,
// brings it back.
import * as maplibregl from "maplibre-gl";
import type * as GeoJSON from "geojson";
import type { NavRouteView, NavSession } from "../lib/nav";
import { useAppStore } from "../store/useAppStore";

/** Following comes back on its own after this long without the user moving the map. */
export const AUTO_RECENTER_MS = 10_000;
/** Heading up only while moving faster than this (standing still, GPS course is noise). */
const HEADING_UP_MPS = 2;

function haversine(a: [number, number], b: [number, number]): number {
  const R = 6_371_008.8;
  const toRad = (d: number) => (d * Math.PI) / 180;
  const dLat = toRad(b[0] - a[0]);
  const dLon = toRad(b[1] - a[1]);
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(toRad(a[0])) * Math.cos(toRad(b[0])) * Math.sin(dLon / 2) ** 2;
  return 2 * R * Math.asin(Math.min(1, Math.sqrt(h)));
}

/** Distance along the route at each shape point. */
export function cumulative(shape: [number, number][]): number[] {
  const out = [0];
  for (let i = 1; i < shape.length; i++) out.push(out[i - 1] + haversine(shape[i - 1], shape[i]));
  return out;
}

/** The route from `progress` metres on, as a GeoJSON line (lon, lat). */
export function remainingLine(shape: [number, number][], cum: number[], progress: number): [number, number][] {
  if (shape.length < 2) return [];
  let i = 0;
  while (i < cum.length - 2 && cum[i + 1] <= progress) i++;
  const len = cum[i + 1] - cum[i];
  const t = len > 0 ? Math.min(1, Math.max(0, (progress - cum[i]) / len)) : 0;
  const a = shape[i];
  const b = shape[i + 1];
  const start: [number, number] = [a[1] + (b[1] - a[1]) * t, a[0] + (b[0] - a[0]) * t];
  return [start, ...shape.slice(i + 1).map(([lat, lon]) => [lon, lat] as [number, number])];
}

/** The navigation route, cameras still ahead, and the destination, for the directions sources. */
export function navFeatures(route: NavRouteView, cum: number[], session: NavSession) {
  const line: GeoJSON.Feature = {
    type: "Feature",
    geometry: { type: "LineString", coordinates: remainingLine(route.shape, cum, session.progress_m) },
    properties: { kind: route.mode, selected: true },
  };
  const cameras: GeoJSON.Feature[] = route.cameras
    .filter((c) => c.along_m > session.progress_m)
    .map((c) => ({ type: "Feature", geometry: { type: "Point", coordinates: [c.lon, c.lat] }, properties: { key: c.key } }));
  const d = session.destination;
  const ends: GeoJSON.Feature[] = [
    { type: "Feature", geometry: { type: "Point", coordinates: [d.lon, d.lat] }, properties: { role: "end", label: "B" } },
  ];
  return { line: [line], cameras, ends };
}

export class NavCamera {
  private marker: maplibregl.Marker;
  private lastTouch = 0;
  private timer: number;
  private offs: (() => void)[] = [];
  private lastBearing = 0;

  constructor(private map: maplibregl.Map) {
    const el = document.createElement("div");
    el.className = "nav-puck";
    el.innerHTML = '<svg viewBox="0 0 40 40" aria-hidden="true"><circle class="halo" cx="20" cy="20" r="18"/><path class="arrow" d="M20 7l9 23-9-5-9 5z"/></svg>';
    this.marker = new maplibregl.Marker({ element: el, rotationAlignment: "map", pitchAlignment: "map" });
    // A user gesture (it carries the DOM event; our own easeTo moves don't) ends following.
    const touched = (e: { originalEvent?: Event }) => {
      if (!e.originalEvent) return;
      this.lastTouch = Date.now();
      if (useAppStore.getState().navFollow) useAppStore.getState().setNavFollow(false);
    };
    for (const type of ["dragstart", "zoomstart", "rotatestart", "pitchstart"] as const) {
      map.on(type, touched);
      this.offs.push(() => map.off(type, touched));
    }
    this.timer = window.setInterval(() => {
      const s = useAppStore.getState();
      if (s.nav && !s.navFollow && Date.now() - this.lastTouch >= AUTO_RECENTER_MS) s.setNavFollow(true);
    }, 1000);
  }

  /** Space the banner and bottom bar take, so the car sits in the open part of the map. */
  private padding(): maplibregl.PaddingOptions {
    const h = this.map.getContainer().clientHeight;
    const rect = (sel: string) => document.querySelector(sel)?.getBoundingClientRect();
    const banner = rect(".nav-banner");
    const bottom = rect(".nav-bottom");
    // The same test as the landscape layout in styles.css (maneuver card on the left).
    const landscape = window.matchMedia("(orientation: landscape) and (max-height: 540px)").matches;
    const top = banner && !landscape ? banner.bottom : 0;
    const bottomPad = bottom ? h - bottom.top + 16 : 16;
    // Heading up: the car low in the view, so more of the road ahead shows.
    const northUp = useAppStore.getState().navNorthUp;
    const lift = northUp ? 0 : Math.max(0, (h - top - bottomPad) * 0.35);
    const left = banner && landscape ? banner.right : 0;
    return { top: top + lift, bottom: bottomPad, left, right: 0 };
  }

  update(session: NavSession) {
    const p = session.position;
    if (!p) return;
    const at: [number, number] = p.snapped ? [p.snapped[1], p.snapped[0]] : [p.lon, p.lat];
    this.marker.setLngLat(at).setRotation(p.bearing);
    if (!this.marker.getElement().isConnected) this.marker.addTo(this.map);
    this.marker.getElement().classList.toggle("weak", session.weak_signal || session.state === "lost_signal" || session.state === "paused");
    const { navFollow, navNorthUp } = useAppStore.getState();
    if (!navFollow) return;
    const moving = p.speed_mps >= HEADING_UP_MPS;
    if (moving) this.lastBearing = p.bearing;
    const zoom = p.speed_mps > 22 ? 15 : p.speed_mps > 12 ? 16 : 16.8;
    this.map.easeTo(
      {
        center: at,
        bearing: navNorthUp ? 0 : this.lastBearing,
        pitch: navNorthUp ? 0 : 45,
        zoom,
        padding: this.padding(),
        duration: 950,
        easing: (t: number) => t,
      },
      { navSource: true },
    );
  }

  /** Back to following straight away (the Recenter button). */
  recenter(session: NavSession | null) {
    this.lastTouch = 0;
    useAppStore.getState().setNavFollow(true);
    if (session) this.update(session);
  }

  destroy() {
    window.clearInterval(this.timer);
    for (const off of this.offs) off();
    this.marker.remove();
    this.map.easeTo({ pitch: 0, bearing: 0, padding: { top: 0, bottom: 0, left: 0, right: 0 }, duration: 600 });
  }
}
