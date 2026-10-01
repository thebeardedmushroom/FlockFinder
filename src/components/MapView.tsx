import type * as GeoJSON from "geojson";
import * as maplibregl from "maplibre-gl";
// MapLibre resolves its worker relative to import.meta.url, which only works for http(s)
// origins. Tauri serves the bundle from tauri://localhost (Linux/macOS) and Vite's dep
// optimizer relocates the module in dev, so hand it a URL that Vite bundles for us.
import maplibreWorkerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import { useEffect, useRef } from "react";

maplibregl.setWorkerUrl(maplibreWorkerUrl);
import { REFRESH_AREA_EVENT, toastError } from "../lib/actions";
import { pointKey } from "../lib/cameraData";
import { primeAudio } from "../lib/chime";
import { getPoints } from "../lib/dataset";
import { sightingVisible } from "../lib/filters";
import { coordLabel, endpointFeatures, routeCameraFeatures, routeFeatures, type RouteChoice } from "../lib/directions";
import { cumulative, NavCamera, navFeatures } from "../map/navCamera";
import { circlePolygon } from "../lib/geo";
import { api } from "../lib/ipc";
import { bandForZoom, WIFI_CLUSTER_THRESHOLD, WIFI_MIN_ZOOM } from "../lib/lod";
import type { BBox, WifiSighting } from "../lib/types";
import { adaptCustomStyle } from "../map/basemap";
import { CameraLayer } from "../map/cameraLayer";
import { buildThemeStyle, DARK_OVERLAY, overlayForStyle, styleBackground, THEME_IDS, THEMES, type OverlayPalette, type ResolvedMapStyle } from "../map/themes";
import { useMapStyle } from "../map/useMapStyle";
import { useAppStore } from "../store/useAppStore";

const BLANK_STYLE: maplibregl.StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#05080f" } }],
};

const SRC_HL = "highlight";
const SRC_AREAS = "areas";
const SRC_ROUTES = "routes";
const SRC_DRAW = "draw";
const SRC_PLAN = "plan";
const SRC_PLAN_CAMS = "plan-cameras";
const SRC_PLAN_ENDS = "plan-ends";
const PLAN_CLICKABLE = ["plan-line", "plan-line-alt"];
const SRC_WIFI_CLUSTERED = "wifi-clustered";
const SRC_WIFI_PLAIN = "wifi-plain";
const EMPTY: GeoJSON.FeatureCollection = { type: "FeatureCollection", features: [] };
const WIFI_CLICKABLE = ["wifi-points-plain", "wifi-points-clustered", "wifi-clusters"];

function fc(features: GeoJSON.Feature[]): GeoJSON.FeatureCollection {
  return { type: "FeatureCollection", features };
}

function sightingFeature(s: WifiSighting): GeoJSON.Feature {
  return {
    type: "Feature",
    geometry: { type: "Point", coordinates: [s.lon, s.lat] },
    properties: { netid: s.netid, imported: s.source === "wigle_import" },
  };
}

/** Our non-camera layers, in the current theme's colours. The camera layer and its hex/stale layers are added separately. */
function ensureLayers(map: maplibregl.Map, pal: OverlayPalette) {
  if (map.getSource(SRC_HL)) return;
  map.addSource(SRC_HL, { type: "geojson", data: EMPTY });
  map.addSource(SRC_AREAS, { type: "geojson", data: EMPTY });
  map.addSource(SRC_ROUTES, { type: "geojson", data: EMPTY });
  map.addSource(SRC_DRAW, { type: "geojson", data: EMPTY });
  map.addSource(SRC_PLAN, { type: "geojson", data: EMPTY });
  map.addSource(SRC_PLAN_CAMS, { type: "geojson", data: EMPTY });
  map.addSource(SRC_PLAN_ENDS, { type: "geojson", data: EMPTY });
  map.addSource(SRC_WIFI_CLUSTERED, {
    type: "geojson",
    data: EMPTY,
    cluster: true,
    clusterRadius: 45,
    clusterMaxZoom: 17,
  });
  map.addSource(SRC_WIFI_PLAIN, { type: "geojson", data: EMPTY });

  const circle = (spec: Record<string, unknown>) =>
    map.addLayer(spec as unknown as maplibregl.CircleLayerSpecification);
  const line = (spec: Record<string, unknown>) =>
    map.addLayer(spec as unknown as maplibregl.LineLayerSpecification);

  // Watch areas: faint cyan fill, dashed hairline outline.
  map.addLayer({
    id: "areas-fill",
    type: "fill",
    source: SRC_AREAS,
    paint: { "fill-color": pal.area, "fill-opacity": 0.04 },
  });
  line({
    id: "areas-line",
    type: "line",
    source: SRC_AREAS,
    paint: { "line-color": pal.area, "line-width": 1.2, "line-dasharray": [4, 3], "line-opacity": 0.75 },
  });
  // Routes: green trace.
  line({
    id: "routes-glow",
    type: "line",
    source: SRC_ROUTES,
    paint: { "line-color": pal.route, "line-width": 9, "line-opacity": 0.12, "line-blur": 4 },
  });
  line({
    id: "routes-line",
    type: "line",
    source: SRC_ROUTES,
    paint: { "line-color": pal.route, "line-width": 2, "line-opacity": 0.9 },
  });
  // Directions: the route not chosen thin, muted and dashed (so it never reads as a road), the
  // chosen one wide, both on a casing so they read over any road colour. Below the camera layer,
  // so markers stay on top.
  const planColor = ["match", ["get", "kind"], "avoid", pal.routeAvoid, pal.routeFast];
  const planLine = (id: string, selected: boolean, width: number, color: unknown, opacity: number, dash?: number[]) =>
    line({
      id,
      type: "line",
      source: SRC_PLAN,
      filter: ["==", ["get", "selected"], selected],
      layout: { "line-join": "round", "line-cap": "round" },
      paint: {
        "line-color": color,
        "line-width": ["interpolate", ["linear"], ["zoom"], 8, width * 0.6, 14, width, 18, width * 1.6],
        "line-opacity": opacity,
        ...(dash ? { "line-dasharray": dash } : {}),
      },
    });
  planLine("plan-casing-alt", false, 7, pal.routeCasing, 0.7);
  planLine("plan-line-alt", false, 4, planColor, 0.9, [1.6, 1.1]);
  planLine("plan-casing", true, 10, pal.routeCasing, 0.9);
  planLine("plan-line", true, 6, planColor, 1);
  // Wi-Fi sightings (suspected devices): sky-blue, smaller, drawn beneath OSM cameras.
  const WIFI = pal.wifi;
  circle({
    id: "wifi-clusters",
    type: "circle",
    source: SRC_WIFI_CLUSTERED,
    filter: ["has", "point_count"],
    paint: {
      "circle-color": WIFI,
      "circle-radius": ["step", ["get", "point_count"], 12, 25, 16, 100, 21],
      "circle-stroke-color": pal.markerInk,
      "circle-stroke-width": 1.5,
      "circle-opacity": 0.75,
    },
  });
  if (map.getStyle()?.glyphs) {
    map.addLayer({
      id: "wifi-cluster-count",
      type: "symbol",
      source: SRC_WIFI_CLUSTERED,
      filter: ["has", "point_count"],
      layout: {
        "text-field": ["get", "point_count_abbreviated"],
        "text-font": ["Noto Sans Regular"],
        "text-size": 11,
        "text-allow-overlap": false,
      },
      paint: { "text-color": pal.markerInk },
    } as unknown as maplibregl.SymbolLayerSpecification);
  }
  const wifiPointPaint = {
    "circle-radius": 4.5,
    "circle-color": WIFI,
    "circle-opacity": 0.85,
    "circle-stroke-color": ["case", ["get", "imported"], pal.wifiImportedStroke, pal.wifiStroke],
    "circle-stroke-width": ["case", ["get", "imported"], 1.5, 1],
  };
  const wifiUnclustered = ["!", ["has", "point_count"]];
  circle({ id: "wifi-points-clustered", type: "circle", source: SRC_WIFI_CLUSTERED, filter: wifiUnclustered, paint: wifiPointPaint });
  circle({ id: "wifi-points-plain", type: "circle", source: SRC_WIFI_PLAIN, paint: wifiPointPaint });
  // Highlight / selection rings (achromatic: selection is a state, not a category).
  circle({
    id: "highlight-glow",
    type: "circle",
    source: SRC_HL,
    paint: {
      "circle-radius": 22,
      "circle-color": ["case", ["get", "sel"], pal.highlightGlow, pal.highlightGlowDim],
      "circle-blur": 1,
    },
  });
  circle({
    id: "highlight",
    type: "circle",
    source: SRC_HL,
    paint: {
      "circle-radius": 13,
      "circle-color": "rgba(0, 0, 0, 0)",
      "circle-stroke-color": ["case", ["get", "sel"], pal.highlight, pal.highlightDim],
      "circle-stroke-width": ["case", ["get", "sel"], 2, 1.25],
    },
  });
  // Cameras still on the chosen route: a warning ring (with a casing) around the marker.
  circle({
    id: "plan-cams-casing",
    type: "circle",
    source: SRC_PLAN_CAMS,
    paint: { "circle-radius": 13, "circle-color": "rgba(0, 0, 0, 0)", "circle-stroke-color": pal.routeCasing, "circle-stroke-width": 5 },
  });
  circle({
    id: "plan-cams",
    type: "circle",
    source: SRC_PLAN_CAMS,
    paint: { "circle-radius": 13, "circle-color": "rgba(0, 0, 0, 0)", "circle-stroke-color": pal.routeCamera, "circle-stroke-width": 2.5 },
  });
  // Start (A, filled) and destination (B, inverted).
  circle({
    id: "plan-ends",
    type: "circle",
    source: SRC_PLAN_ENDS,
    paint: {
      "circle-radius": 9,
      "circle-color": ["match", ["get", "role"], "start", pal.highlight, pal.routeCasing],
      "circle-stroke-color": ["match", ["get", "role"], "start", pal.routeCasing, pal.highlight],
      "circle-stroke-width": 2.5,
    },
  });
  if (map.getStyle()?.glyphs) {
    map.addLayer({
      id: "plan-ends-label",
      type: "symbol",
      source: SRC_PLAN_ENDS,
      layout: { "text-field": ["get", "label"], "text-font": ["Noto Sans Bold"], "text-size": 11, "text-allow-overlap": true, "text-ignore-placement": true },
      paint: { "text-color": ["match", ["get", "role"], "start", pal.routeCasing, pal.highlight] },
    } as unknown as maplibregl.SymbolLayerSpecification);
  }
  // Route being drawn: violet dashes, on top of everything.
  line({
    id: "draw-line",
    type: "line",
    source: SRC_DRAW,
    filter: ["==", ["geometry-type"], "LineString"],
    paint: { "line-color": pal.draw, "line-width": 2.5, "line-dasharray": [2, 1.5] },
  });
  circle({
    id: "draw-points",
    type: "circle",
    source: SRC_DRAW,
    filter: ["==", ["geometry-type"], "Point"],
    paint: { "circle-radius": 5, "circle-color": pal.draw, "circle-stroke-color": pal.markerInk, "circle-stroke-width": 1.5 },
  });
}

function setData(map: maplibregl.Map, id: string, data: GeoJSON.FeatureCollection) {
  const src = map.getSource(id) as maplibregl.GeoJSONSource | undefined;
  src?.setData(data);
}

/** Push the non-camera store state into the map sources. */
function syncData(map: maplibregl.Map) {
  const s = useAppStore.getState();
  const wifi: GeoJSON.Feature[] = [];
  if (s.zoom >= WIFI_MIN_ZOOM) {
    for (const w of Object.values(s.sightings)) if (sightingVisible(w, s.filters)) wifi.push(sightingFeature(w));
  }
  if (wifi.length > WIFI_CLUSTER_THRESHOLD) {
    setData(map, SRC_WIFI_CLUSTERED, fc(wifi));
    setData(map, SRC_WIFI_PLAIN, EMPTY);
  } else {
    setData(map, SRC_WIFI_CLUSTERED, EMPTY);
    setData(map, SRC_WIFI_PLAIN, fc(wifi));
  }

  const hl: GeoJSON.Feature[] = s.highlighted.map((c) => ({
    type: "Feature",
    geometry: { type: "Point", coordinates: [c.lon, c.lat] },
    properties: { sel: false },
  }));
  const sel =
    s.selection?.kind === "camera"
      ? s.selection.camera
      : s.selection?.kind === "submission"
        ? s.selection.submission
        : s.selection?.kind === "wifi"
          ? s.selection.sighting
          : null;
  if (sel) hl.push({ type: "Feature", geometry: { type: "Point", coordinates: [sel.lon, sel.lat] }, properties: { sel: true } });
  setData(map, SRC_HL, fc(hl));

  const areas: GeoJSON.Feature[] = (s.alertState?.areas ?? []).map((a) => ({
    type: "Feature",
    geometry: { type: "Polygon", coordinates: [circlePolygon(a.area.lat, a.area.lon, a.area.radius_m)] },
    properties: { id: a.area.id, name: a.area.name },
  }));
  setData(map, SRC_AREAS, fc(areas));

  const routes: GeoJSON.Feature[] = [];
  for (const r of s.alertState?.routes ?? []) {
    try {
      const geom = JSON.parse(r.route.geojson) as GeoJSON.Geometry;
      routes.push({ type: "Feature", geometry: geom, properties: { id: r.route.id, name: r.route.name } });
    } catch {
      /* ignore malformed */
    }
  }
  setData(map, SRC_ROUTES, fc(routes));

  const draw: GeoJSON.Feature[] = s.drawPoints.map((p) => ({
    type: "Feature",
    geometry: { type: "Point", coordinates: [p[1], p[0]] },
    properties: {},
  }));
  if (s.drawPoints.length >= 2) {
    draw.push({
      type: "Feature",
      geometry: { type: "LineString", coordinates: s.drawPoints.map((p) => [p[1], p[0]]) },
      properties: {},
    });
  }
  setData(map, SRC_DRAW, fc(draw));

  syncPlan(map);
}

/** Shape distances of the navigation route drawn last (by route version). */
let navCum: { version: number; cum: number[] } | null = null;

/** The directions layers: the navigation route while navigating, else the planned routes. */
function syncPlan(map: maplibregl.Map) {
  const s = useAppStore.getState();
  if (s.nav && s.navRoute) {
    if (navCum?.version !== s.navRoute.version) navCum = { version: s.navRoute.version, cum: cumulative(s.navRoute.shape) };
    const f = navFeatures(s.navRoute, navCum.cum, s.nav);
    setData(map, SRC_PLAN, fc(f.line));
    setData(map, SRC_PLAN_CAMS, fc(f.cameras));
    setData(map, SRC_PLAN_ENDS, fc(f.ends));
    return;
  }
  const d = s.directions;
  setData(map, SRC_PLAN, fc(d.plan ? routeFeatures(d.plan, d.selected) : []));
  setData(map, SRC_PLAN_CAMS, fc(d.plan ? routeCameraFeatures(d.plan, d.plan.same_route ? "avoid" : d.selected) : []));
  setData(map, SRC_PLAN_ENDS, fc(endpointFeatures(d.start, d.end)));
}

/** The style to hand MapLibre for a resolved theme (custom URLs are adapted as they load). */
function styleFor(r: ResolvedMapStyle): maplibregl.StyleSpecification | string {
  if (r.kind === "theme") return buildThemeStyle(r.theme);
  return r.url || BLANK_STYLE;
}

/**
 * Swap the basemap. The camera stays put (themes carry no camera of their own) and the
 * `style.load` handler re-adds our sources and layers to the fresh style.
 */
function applyStyle(map: maplibregl.Map, r: ResolvedMapStyle, onOverlay: (o: OverlayPalette) => void): void {
  const style = styleFor(r);
  if (typeof style !== "string") {
    map.setStyle(style, { diff: false });
    publishScheme(r, style);
    return;
  }
  // A custom style's ground is only known once it has loaded; this runs before `style.load`.
  map.setStyle(style, {
    diff: false,
    transformStyle: (_prev, next) => {
      const out = adaptCustomStyle(next);
      const overlay = overlayForStyle(out);
      onOverlay(overlay);
      publishScheme({ ...r, overlay, scheme: overlay.scheme }, out);
      return out;
    },
  });
  publishScheme(r, null);
}

/** Tell the page chrome (vignette, location dot, map background) what ground it sits on. */
function publishScheme(r: ResolvedMapStyle, style: maplibregl.StyleSpecification | null): void {
  const root = document.documentElement;
  root.dataset.mapScheme = r.scheme;
  root.dataset.mapTheme = r.kind === "theme" ? r.theme.id : "custom";
  const bg = style ? styleBackground(style) : null;
  if (bg) root.style.setProperty("--map-bg", bg);
  else root.style.removeProperty("--map-bg");
}

/** Map button that steps through the themes (Settings has the full list). */
class ThemeControl implements maplibregl.IControl {
  private el: HTMLDivElement | null = null;
  constructor(private readonly onPress: () => void) {}

  onAdd(): HTMLElement {
    const el = document.createElement("div");
    el.className = "maplibregl-ctrl maplibregl-ctrl-group";
    const button = document.createElement("button");
    button.type = "button";
    button.className = "ff-theme-toggle";
    button.title = "Next map theme";
    button.setAttribute("aria-label", "Next map theme");
    // Half-filled disc: the usual "appearance" glyph.
    button.innerHTML =
      '<svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true"><circle cx="10" cy="10" r="6.5" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M10 3.5a6.5 6.5 0 0 1 0 13z" fill="currentColor"/></svg>';
    button.addEventListener("click", this.onPress);
    el.appendChild(button);
    this.el = el;
    return el;
  }

  onRemove(): void {
    this.el?.remove();
    this.el = null;
  }
}

function cycleTheme(currentKey: string | null): void {
  const store = useAppStore.getState();
  const i = THEME_IDS.indexOf(currentKey as (typeof THEME_IDS)[number]);
  const next = THEME_IDS[(i + 1) % THEME_IDS.length];
  store.setMapTheme(next);
  // One toast for a run of presses, not one per press.
  for (const t of store.toasts) if (t.text.startsWith("Map theme:")) store.dismissToast(t.id);
  store.pushToast(`Map theme: ${THEMES[next].name}`, "info");
}

/**
 * MapLibre's locate control, except that following keeps your zoom. The stock control re-fits
 * the view to the accuracy circle on every fix (zoom 15 at most), and a phone reports a fix
 * every second or so, so a pinch zoom snapped back almost at once. Here only the first fix
 * after a press of the button fits the view; later fixes just re-centre, keeping zoom and
 * bearing, and skip a fix while a zoom or rotation is under way.
 */
class FollowingGeolocateControl extends maplibregl.GeolocateControl {
  /** The next camera update fits the view to the fix (set by a press of the button). */
  private fitNext = true;

  constructor(options: ConstructorParameters<typeof maplibregl.GeolocateControl>[0]) {
    super(options);
    const fit = this._updateCamera;
    this._updateCamera = (position) => {
      if (this.fitNext) {
        this.fitNext = false;
        fit(position);
        return;
      }
      // Mid pinch or rotate: leave the camera to the gesture (a drag already ends following).
      // A re-centre still running from the previous fix is simply replaced.
      if (this._map.isZooming() || this._map.isRotating()) return;
      // Tagged like the stock update, so the move doesn't drop the control out of follow mode.
      this._map.easeTo({ center: [position.coords.longitude, position.coords.latitude], duration: 600 }, { geolocateSource: true });
    };
  }

  onAdd(map: maplibregl.Map): HTMLElement {
    const el = super.onAdd(map);
    // Capture phase: runs before the button's own handler, which may update the camera at once.
    el.addEventListener("click", () => (this.fitNext = true), true);
    return el;
  }
}

/** `code` follows the Geolocation API: 1 denied, 2 unavailable, 3 timeout. */
function geolocationMessage(err: { code: number; message: string }): string {
  switch (err.code) {
    case 1:
      return "Location permission was denied. Allow location access for Flock Finder in your system settings to show your position.";
    case 2:
      return "Your position is unavailable. Check that location services are turned on.";
    case 3:
      return "Timed out waiting for a location fix. Try again.";
    default:
      return `Could not get your location: ${err.message}`;
  }
}

function boundsToBBox(map: maplibregl.Map): BBox {
  const b = map.getBounds();
  return { south: b.getSouth(), west: b.getWest(), north: b.getNorth(), east: b.getEast() };
}

async function selectCameraAt(index: number) {
  const key = pointKey(getPoints(), index);
  try {
    const [camera] = await api.getCamerasByKeys([key]);
    if (camera) useAppStore.getState().select({ kind: "camera", camera });
  } catch (e) {
    toastError(e, "Could not open camera");
  }
}

export default function MapView() {
  const containerRef = useRef<HTMLDivElement>(null);
  const mapRef = useRef<maplibregl.Map | null>(null);
  const layerRef = useRef<CameraLayer | null>(null);
  const readyRef = useRef(false);
  const saveRef = useRef<number | null>(null);
  const wifiTimer = useRef<number | null>(null);
  const wifiSeq = useRef(0);
  const markerRef = useRef<maplibregl.Marker | null>(null);
  /** The pin of a searched place (or dropped pin) whose detail is open. */
  const placeMarkerRef = useRef<maplibregl.Marker | null>(null);
  /** The theme (or custom URL) the map is on, so the style effect doesn't re-apply it on mount. */
  const styleKeyRef = useRef<string | null>(null);
  /** Overlay colours for the theme the map is on; read whenever our layers are (re)built. */
  const palRef = useRef<OverlayPalette>(DARK_OVERLAY);

  const view = useAppStore((s) => s.view);
  const mapStyle = useMapStyle();
  const setOverlay = (o: OverlayPalette) => {
    palRef.current = o;
    layerRef.current?.setPalette(o);
  };
  const mode = useAppStore((s) => s.mode);
  const draftPin = useAppStore((s) => s.draftPin);
  const fly = useAppStore((s) => s.fly);
  // Values whose change should re-sync the non-camera map sources.
  const submissions = useAppStore((s) => s.submissions);
  const filters = useAppStore((s) => s.filters);
  const zoom = useAppStore((s) => s.zoom);
  const highlighted = useAppStore((s) => s.highlighted);
  const selection = useAppStore((s) => s.selection);
  const alertState = useAppStore((s) => s.alertState);
  const drawPoints = useAppStore((s) => s.drawPoints);
  const sightings = useAppStore((s) => s.sightings);
  const datasetVersion = useAppStore((s) => s.dataset?.version ?? 0);
  const directions = useAppStore((s) => s.directions);
  const nav = useAppStore((s) => s.nav);
  const navRoute = useAppStore((s) => s.navRoute);
  const navFollow = useAppStore((s) => s.navFollow);
  const navNorthUp = useAppStore((s) => s.navNorthUp);
  const navActive = nav !== null;
  const navCameraRef = useRef<NavCamera | null>(null);

  /** Wi-Fi sightings stay a per-viewport layer (local table only) from WIFI_MIN_ZOOM. */
  const loadWifi = async () => {
    const map = mapRef.current;
    if (!map) return;
    const store = useAppStore.getState();
    const seq = ++wifiSeq.current;
    if (map.getZoom() < WIFI_MIN_ZOOM || !store.filters.wifi) {
      if (Object.keys(store.sightings).length > 0) store.setSightings([]);
      return;
    }
    try {
      const r = await api.getWifiSightings(boundsToBBox(map));
      if (seq === wifiSeq.current) store.setSightings(r);
    } catch (e) {
      toastError(e, "Could not load Wi-Fi sightings");
    }
  };

  const scheduleWifi = () => {
    if (wifiTimer.current) window.clearTimeout(wifiTimer.current);
    wifiTimer.current = window.setTimeout(() => void loadWifi(), 300);
  };

  const scheduleSaveView = () => {
    if (saveRef.current) window.clearTimeout(saveRef.current);
    saveRef.current = window.setTimeout(() => {
      const map = mapRef.current;
      if (!map) return;
      const c = map.getCenter();
      void api.saveView({ lat: c.lat, lon: c.lng, zoom: map.getZoom() }).catch(() => {});
    }, 1000);
  };

  // Create the map once the initial view is known.
  useEffect(() => {
    if (!containerRef.current || mapRef.current || !view || !mapStyle) return;
    const initial = styleFor(mapStyle);
    const map = new maplibregl.Map({
      container: containerRef.current,
      // A custom URL goes through applyStyle below, which adapts it as it loads.
      style: typeof initial === "string" ? BLANK_STYLE : initial,
      center: [view.lon, view.lat],
      zoom: view.zoom,
      attributionControl: false,
      maxZoom: 19,
      // By default MapLibre rasterises Chinese/Japanese/Korean glyphs itself, on the main
      // thread (TinySDF: canvas measureText + getImageData per glyph). Panning the wide band
      // over East Asia then stalled 40–50 ms per batch of new labels. The style's glyph server
      // (OpenFreeMap's Noto Sans) covers those ranges, so fetch them like every other glyph.
      localIdeographFontFamily: false,
    });
    mapRef.current = map;
    styleKeyRef.current = mapStyle.key;
    palRef.current = mapStyle.overlay;
    if (typeof initial === "string") applyStyle(map, mapStyle, (o) => setOverlay(o));
    else publishScheme(mapStyle, initial);
    const layer = new CameraLayer({
      onLod: (s) => useAppStore.getState().setLod(s),
      onCounts: (c) => useAppStore.getState().setInView(c),
      onPickCamera: (i) => void selectCameraAt(i),
      onPickSubmission: (id) => {
        const store = useAppStore.getState();
        const submission = store.submissions.find((s) => s.id === id);
        if (submission) store.select({ kind: "submission", submission });
      },
      onBuilt: (info) => {
        if (info.first) performance.mark("ff:index-built");
      },
    });
    layer.setPalette(mapStyle.overlay);
    layerRef.current = layer;
    if (import.meta.env.DEV || location.search.includes("debug")) {
      (window as unknown as Record<string, unknown>).__ff = { map, layer, store: useAppStore, points: getPoints };
    }

    const compactAttribution = window.matchMedia("(max-width: 720px)").matches;
    map.addControl(
      new maplibregl.AttributionControl({
        compact: compactAttribution,
        customAttribution: "© OpenStreetMap contributors (ODbL)",
      }),
      "bottom-right",
    );
    // The attribution is a full-width bar at the bottom on a phone, and MapLibre re-expands it
    // whenever the credits change. Publish its height on the document root (the HUD and legend
    // are siblings of the map, so they cannot inherit it from the map container) and keep it
    // current, so they sit above the bar whether it is expanded or collapsed to its ⓘ button.
    const corner = map.getContainer().querySelector(".maplibregl-ctrl-bottom-right");
    const publishAttribHeight = () => {
      const el = map.getContainer().querySelector(".maplibregl-ctrl-attrib");
      // Folded to its ⓘ button it sits beside the zoom buttons, clear of the HUD.
      const folded = el?.classList.contains("maplibregl-compact") && !el.classList.contains("maplibregl-compact-show");
      // How much of the map's bottom edge the bar covers, including MapLibre's own inset.
      const space = el && !folded ? Math.round(map.getContainer().getBoundingClientRect().bottom - el.getBoundingClientRect().top) : 0;
      document.documentElement.style.setProperty("--attrib-space", `${Math.max(0, space)}px`);
    };
    let attribObserver: ResizeObserver | null = null;
    if (corner && "ResizeObserver" in window) {
      attribObserver = new ResizeObserver(publishAttribHeight);
      attribObserver.observe(corner);
    }
    publishAttribHeight();
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "bottom-right");
    // "Locate me" via the WebView's Geolocation API, only while the button is on. The position
    // lives in this map (dot, optional follow); it is never stored or sent to the backend.
    const geolocate = new FollowingGeolocateControl({
      positionOptions: { enableHighAccuracy: true, timeout: 15000, maximumAge: 10000 },
      trackUserLocation: true,
      showAccuracyCircle: true,
      fitBoundsOptions: { maxZoom: 15 },
    });
    geolocate.on("error", (err) => {
      useAppStore.getState().pushToast(geolocationMessage(err), "warn");
    });
    // Positions feed the proximity alerts (in memory only).
    geolocate.on("geolocate", (e) => {
      useAppStore.getState().setUserPosition({
        lat: e.coords.latitude,
        lon: e.coords.longitude,
        accuracy: e.coords.accuracy,
        speed: e.coords.speed,
        at: e.timestamp,
      });
    });
    map.addControl(geolocate, "bottom-right");
    map.addControl(new ThemeControl(() => cycleTheme(styleKeyRef.current)), "bottom-right");
    // MapLibre has no "switched off" event, so follow the button's state classes instead.
    const geoButton = map.getContainer().querySelector<HTMLButtonElement>(".maplibregl-ctrl-geolocate");
    if (geoButton) {
      const syncLocate = () =>
        useAppStore
          .getState()
          .setLocateActive(/maplibregl-ctrl-geolocate-(active|background|waiting)/.test(geoButton.className));
      new MutationObserver(syncLocate).observe(geoButton, { attributes: true, attributeFilter: ["class"] });
      // The press is the user gesture that lets a later alert play its sound.
      geoButton.addEventListener("pointerdown", primeAudio);
    }

    useAppStore.getState().setViewport(boundsToBBox(map), map.getZoom());

    /** The custom style URL a blank fallback was already shown for (once per URL). */
    let fallbackFor: string | null = null;
    map.on("style.load", () => {
      // The density field goes above every basemap fill and line but under the trailing block
      // of labels, so place names stay readable on it. (Styles can interleave an early symbol
      // layer among the fills; "first symbol" would bury the field under water and landuse.)
      const styleLayers = map.getStyle()?.layers ?? [];
      let lastGeometry = -1;
      styleLayers.forEach((l, i) => {
        if (l.type !== "symbol") lastGeometry = i;
      });
      const labelsFrom = styleLayers[lastGeometry + 1]?.id;
      ensureLayers(map, palRef.current);
      layer.installStyleLayers(map, labelsFrom ?? "areas-fill");
      if (!map.getLayer(layer.id)) map.addLayer(layer, "highlight-glow");
      readyRef.current = true;
      syncData(map);
    });
    map.on("load", () => void loadWifi());
    map.on("error", (e) => {
      const msg = (e as { error?: { message?: string } }).error?.message ?? "";
      if (/glyph|font|sprite/i.test(msg)) return; // cosmetic with a blank style
      // If a custom style URL cannot be fetched (offline, bad URL), fall back to a blank
      // style so `load` still fires and cameras render. Themes are bundled: their style
      // always loads, and failed tiles just leave the theme's background showing.
      const key = styleKeyRef.current;
      if (!readyRef.current && key?.startsWith("custom:") && fallbackFor !== key && !map.isStyleLoaded()) {
        fallbackFor = key;
        const store = useAppStore.getState();
        store.pushToast("Basemap style could not be loaded (offline or bad URL). Showing cameras on a blank background.", "warn");
        if (/fetch|network|Failed|load/i.test(msg)) store.setOffline(true);
        map.setStyle(BLANK_STYLE);
        return;
      }
      console.warn("map error", msg);
    });
    map.on("moveend", () => {
      useAppStore.getState().setViewport(boundsToBBox(map), map.getZoom());
      scheduleWifi();
      scheduleSaveView();
    });

    // Touch has no right-click: a ~0.5 s press opens the same context menu. The tap that
    // ends a long press must not also count as a click (which would close the menu).
    let press: { timer: number; x: number; y: number } | null = null;
    let pressFired = false;
    const cancelPress = () => {
      if (press) window.clearTimeout(press.timer);
      press = null;
    };
    map.on("touchstart", (e) => {
      cancelPress();
      pressFired = false;
      if (e.originalEvent.touches.length !== 1) return;
      const { point, lngLat } = e;
      press = {
        x: point.x,
        y: point.y,
        timer: window.setTimeout(() => {
          press = null;
          pressFired = true;
          useAppStore.getState().setContextMenu({ x: point.x, y: point.y, lat: lngLat.lat, lon: lngLat.lng });
        }, 550),
      };
    });
    map.on("touchmove", (e) => {
      if (press && Math.hypot(e.point.x - press.x, e.point.y - press.y) > 10) cancelPress();
    });
    map.on("touchend", cancelPress);
    map.on("touchcancel", cancelPress);

    const coarse = window.matchMedia("(pointer: coarse)");
    map.on("click", (e) => {
      if (pressFired) {
        pressFired = false;
        return;
      }
      const store = useAppStore.getState();
      if (store.contextMenu) store.setContextMenu(null);
      if (store.mode === "add") {
        store.setDraftPin({ lat: e.lngLat.lat, lon: e.lngLat.lng });
        return;
      }
      if (store.mode === "draw") {
        store.addDrawPoint([e.lngLat.lat, e.lngLat.lng]);
        return;
      }
      if (store.mode === "pick") {
        const which = store.directions.picking ?? "start";
        store.setEndpoint(which, { lat: e.lngLat.lat, lon: e.lngLat.lng, label: coordLabel(e.lngLat.lat, e.lngLat.lng) });
        store.setMode("view");
        return;
      }
      if (layer.handleClick(e.point.x, e.point.y, coarse.matches)) return;
      const layers = WIFI_CLICKABLE.filter((id) => map.getLayer(id));
      const hit = layers.length ? map.queryRenderedFeatures(e.point, { layers })[0] : undefined;
      if (!hit) {
        // A click on a directions route selects it.
        const planLayers = PLAN_CLICKABLE.filter((id) => map.getLayer(id));
        const route = planLayers.length ? map.queryRenderedFeatures(e.point, { layers: planLayers })[0] : undefined;
        if (route && store.directions.plan && !store.directions.plan.same_route) {
          store.setDirections({ selected: route.properties?.kind as RouteChoice });
          return;
        }
        store.select(null);
        return;
      }
      if (hit.layer.id === "wifi-clusters") {
        const src = map.getSource(SRC_WIFI_CLUSTERED) as maplibregl.GeoJSONSource;
        const clusterId = hit.properties?.cluster_id as number;
        void src.getClusterExpansionZoom(clusterId).then((z) => {
          const [lon, lat] = (hit.geometry as GeoJSON.Point).coordinates;
          map.easeTo({ center: [lon, lat], zoom: Math.min(z, 19) });
        });
        return;
      }
      const sighting = store.sightings[hit.properties?.netid as string];
      if (sighting) store.select({ kind: "wifi", sighting });
    });
    map.on("dblclick", (e) => {
      const store = useAppStore.getState();
      if (store.mode === "draw") {
        e.preventDefault();
        if (store.drawPoints.length >= 2) {
          store.setPendingRoute({ points: store.drawPoints, name: "", lengthM: null, source: "draw" });
          store.setMode("view");
        }
      }
    });
    map.on("contextmenu", (e) => {
      e.preventDefault();
      useAppStore.getState().setContextMenu({ x: e.point.x, y: e.point.y, lat: e.lngLat.lat, lon: e.lngLat.lng });
    });
    // Pointer feedback and the hex-cell readout, at most once per frame.
    let hoverFrame = 0;
    map.on("mousemove", (e) => {
      if (hoverFrame) return;
      const { x, y } = e.point;
      hoverFrame = requestAnimationFrame(() => {
        hoverFrame = 0;
        const store = useAppStore.getState();
        let hex: { count: number; users: number } | null = null;
        if (bandForZoom(map.getZoom()) === "wide") {
          const ll = map.unproject([x, y]);
          hex = layer.hexAt(ll.lng, ll.lat);
        }
        store.setHexHover(hex);
        if (store.mode !== "view") return;
        const wifiLayers = WIFI_CLICKABLE.filter((id) => map.getLayer(id));
        const planLayers = store.directions.plan && !store.directions.plan.same_route ? PLAN_CLICKABLE.filter((id) => map.getLayer(id)) : [];
        const hoverable = [...wifiLayers, ...planLayers];
        const over = layer.hoverTest(x, y) || (hoverable.length > 0 && map.queryRenderedFeatures([x, y], { layers: hoverable }).length > 0);
        map.getCanvas().style.cursor = over ? "pointer" : "";
      });
    });
    map.on("mouseout", () => useAppStore.getState().setHexHover(null));

    const onRefresh = () => void loadWifi();
    // Re-query on focus so sightings added while the window was in the background (the
    // flockfinder-wifi-sync CLI) show up without a restart.
    const onFocus = () => void loadWifi();
    window.addEventListener(REFRESH_AREA_EVENT, onRefresh);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener(REFRESH_AREA_EVENT, onRefresh);
      window.removeEventListener("focus", onFocus);
      attribObserver?.disconnect();
      layer.destroy();
      layerRef.current = null;
      map.remove();
      mapRef.current = null;
      readyRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view !== null, mapStyle !== null]);

  // Theme (or custom style URL) changes. Rapid switching is safe: each setStyle abandons the
  // previous style, and our layers are only ever added to a fresh one.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !mapStyle || mapStyle.key === styleKeyRef.current) return;
    styleKeyRef.current = mapStyle.key;
    setOverlay(mapStyle.overlay);
    readyRef.current = false;
    applyStyle(map, mapStyle, setOverlay);
  }, [mapStyle]);

  // New camera snapshot or submissions: re-index (worker).
  useEffect(() => {
    const layer = layerRef.current;
    if (!layer || datasetVersion === 0) return;
    layer.setData(getPoints(), submissions);
  }, [datasetVersion, submissions]);

  // Filters: counts are recomputed from the filtered set, never reused.
  useEffect(() => {
    const layer = layerRef.current;
    if (!layer) return;
    layer.setFilter({ flock: filters.flock, alpr: filters.alpr, user: filters.user, operator: filters.operator });
    layer.setCones(filters.cones);
    void loadWifi();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filters]);

  // Re-sync the other sources whenever relevant state changes.
  useEffect(() => {
    const map = mapRef.current;
    if (map && readyRef.current) syncData(map);
  }, [sightings, filters, zoom, highlighted, selection, alertState, drawPoints, directions]);

  // Navigation: the car marker and following camera live as long as the session.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !navActive) return;
    const cam = new NavCamera(map);
    navCameraRef.current = cam;
    map.getContainer().classList.add("navigating");
    return () => {
      cam.destroy();
      navCameraRef.current = null;
      map.getContainer().classList.remove("navigating");
      if (readyRef.current) syncPlan(map);
    };
  }, [navActive]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !nav) return;
    if (readyRef.current) syncPlan(map);
    navCameraRef.current?.update(nav);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nav, navRoute, navFollow, navNorthUp]);

  // Mode cursor + double-click zoom.
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    map.getCanvas().style.cursor = mode === "view" ? "" : "crosshair";
    if (mode === "draw") map.doubleClickZoom.disable();
    else map.doubleClickZoom.enable();
  }, [mode]);

  // Draggable draft pin in add mode.
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    if (mode !== "add" || !draftPin) {
      markerRef.current?.remove();
      markerRef.current = null;
      return;
    }
    if (!markerRef.current) {
      const el = document.createElement("div");
      el.className = "marker-draft";
      const m = new maplibregl.Marker({ element: el, draggable: true, anchor: "bottom" })
        .setLngLat([draftPin.lon, draftPin.lat])
        .addTo(map);
      m.on("dragend", () => {
        const p = m.getLngLat();
        useAppStore.getState().setDraftPin({ lat: p.lat, lon: p.lng });
      });
      markerRef.current = m;
    } else {
      markerRef.current.setLngLat([draftPin.lon, draftPin.lat]);
    }
  }, [mode, draftPin]);

  // The selected place's pin.
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    const place = selection?.kind === "place" ? selection.place : null;
    if (!place) {
      placeMarkerRef.current?.remove();
      placeMarkerRef.current = null;
      return;
    }
    if (!placeMarkerRef.current) {
      const el = document.createElement("div");
      el.className = "marker-place";
      el.appendChild(document.createElement("div")).className = "marker-place-pin";
      placeMarkerRef.current = new maplibregl.Marker({ element: el, anchor: "bottom" }).setLngLat([place.lon, place.lat]).addTo(map);
    } else {
      placeMarkerRef.current.setLngLat([place.lon, place.lat]);
    }
  }, [selection]);

  // Fly requests.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !fly) return;
    if (fly.bbox) {
      // Keep the box clear of an open left panel (a bottom sheet on a phone).
      const padding = { top: 80, bottom: 80, left: 80, right: 80 };
      const panelEl = document.querySelector<HTMLElement>(".panel:not(.right)");
      if (panelEl) {
        const p = panelEl.getBoundingClientRect();
        const c = map.getContainer().getBoundingClientRect();
        if (p.width >= c.width * 0.9) padding.bottom = Math.max(80, Math.min(c.height - 200, c.bottom - p.top + 30));
        else padding.left = Math.max(80, Math.min(c.width - 200, p.right - c.left + 40));
      }
      map.fitBounds(
        [
          [fly.bbox.west, fly.bbox.south],
          [fly.bbox.east, fly.bbox.north],
        ],
        { padding, maxZoom: 16, duration: 900 },
      );
    } else if (fly.lat !== undefined && fly.lon !== undefined) {
      map.flyTo({ center: [fly.lon, fly.lat], zoom: fly.zoom ?? Math.max(map.getZoom(), 14), duration: 900 });
    }
  }, [fly]);

  return (
    <>
      <div ref={containerRef} className={`map mode-${mode}`} />
      {mapStyle?.kind === "custom" && mapStyle.url === "" && (
        <div className="no-style">
          <div className="callout info">
            <strong>No basemap configured.</strong> Cameras still show on this blank background.
            Open Settings and pick a map theme, or paste a MapLibre style URL.
          </div>
        </div>
      )}
    </>
  );
}
