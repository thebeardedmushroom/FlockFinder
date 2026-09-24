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
import { MARKER_COLORS } from "../lib/classify";
import { getPoints } from "../lib/dataset";
import { sightingVisible } from "../lib/filters";
import { circlePolygon } from "../lib/geo";
import { api } from "../lib/ipc";
import { bandForZoom, WIFI_CLUSTER_THRESHOLD, WIFI_MIN_ZOOM } from "../lib/lod";
import type { BBox, WifiSighting } from "../lib/types";
import { tameBasemap } from "../map/basemap";
import { CameraLayer } from "../map/cameraLayer";
import { useAppStore } from "../store/useAppStore";

const BLANK_STYLE: maplibregl.StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#05080f" } }],
};

// Overlay palette for user-drawn geometry (kept in sync with styles.css tokens).
const CYAN = "#00e5ff";
const GREEN = "#3dffa7";
const VIOLET = "#a78bfa";
const INK = "#05080f";

const SRC_HL = "highlight";
const SRC_AREAS = "areas";
const SRC_ROUTES = "routes";
const SRC_DRAW = "draw";
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

/** Our non-camera layers. The camera layer and its hex/stale layers are added separately. */
function ensureLayers(map: maplibregl.Map) {
  if (map.getSource(SRC_HL)) return;
  map.addSource(SRC_HL, { type: "geojson", data: EMPTY });
  map.addSource(SRC_AREAS, { type: "geojson", data: EMPTY });
  map.addSource(SRC_ROUTES, { type: "geojson", data: EMPTY });
  map.addSource(SRC_DRAW, { type: "geojson", data: EMPTY });
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
    paint: { "fill-color": CYAN, "fill-opacity": 0.04 },
  });
  line({
    id: "areas-line",
    type: "line",
    source: SRC_AREAS,
    paint: { "line-color": CYAN, "line-width": 1.2, "line-dasharray": [4, 3], "line-opacity": 0.75 },
  });
  // Routes: green trace.
  line({
    id: "routes-glow",
    type: "line",
    source: SRC_ROUTES,
    paint: { "line-color": GREEN, "line-width": 9, "line-opacity": 0.12, "line-blur": 4 },
  });
  line({
    id: "routes-line",
    type: "line",
    source: SRC_ROUTES,
    paint: { "line-color": GREEN, "line-width": 2, "line-opacity": 0.9 },
  });
  // Wi-Fi sightings (suspected devices): sky-blue, smaller, drawn beneath OSM cameras.
  const WIFI = MARKER_COLORS.wifi;
  circle({
    id: "wifi-clusters",
    type: "circle",
    source: SRC_WIFI_CLUSTERED,
    filter: ["has", "point_count"],
    paint: {
      "circle-color": WIFI,
      "circle-radius": ["step", ["get", "point_count"], 12, 25, 16, 100, 21],
      "circle-stroke-color": INK,
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
      paint: { "text-color": INK },
    } as unknown as maplibregl.SymbolLayerSpecification);
  }
  const wifiPointPaint = {
    "circle-radius": 4.5,
    "circle-color": WIFI,
    "circle-opacity": 0.85,
    "circle-stroke-color": ["case", ["get", "imported"], "#f8fafc", INK],
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
      "circle-color": ["case", ["get", "sel"], "rgba(255, 255, 255, 0.14)", "rgba(255, 255, 255, 0.1)"],
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
      "circle-stroke-color": ["case", ["get", "sel"], "#ffffff", "rgba(255, 255, 255, 0.7)"],
      "circle-stroke-width": ["case", ["get", "sel"], 2, 1.25],
    },
  });
  // Route being drawn: violet dashes, on top of everything.
  line({
    id: "draw-line",
    type: "line",
    source: SRC_DRAW,
    filter: ["==", ["geometry-type"], "LineString"],
    paint: { "line-color": VIOLET, "line-width": 2.5, "line-dasharray": [2, 1.5] },
  });
  circle({
    id: "draw-points",
    type: "circle",
    source: SRC_DRAW,
    filter: ["==", ["geometry-type"], "Point"],
    paint: { "circle-radius": 5, "circle-color": VIOLET, "circle-stroke-color": INK, "circle-stroke-width": 1.5 },
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
  /** The style the map is currently on, so the style effect doesn't re-apply it on mount. */
  const styleRef = useRef<string | undefined>(undefined);

  const view = useAppStore((s) => s.view);
  const styleUrl = useAppStore((s) => s.settings?.style_url);
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
    if (!containerRef.current || mapRef.current || !view || styleUrl === undefined) return;
    const map = new maplibregl.Map({
      container: containerRef.current,
      style: styleUrl ? styleUrl : BLANK_STYLE,
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
    styleRef.current = styleUrl;
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
      // How much of the map's bottom edge the bar covers, including MapLibre's own inset.
      const space = el ? Math.round(map.getContainer().getBoundingClientRect().bottom - el.getBoundingClientRect().top) : 0;
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
    const geolocate = new maplibregl.GeolocateControl({
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
        at: e.timestamp,
      });
    });
    map.addControl(geolocate, "bottom-right");
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

    let styleFallbackDone = false;
    map.on("style.load", () => {
      // A fresh style never contains our layers; if they are still here this is a repeat
      // event for a style we already adjusted, and muting it again would compound.
      if (!map.getLayer("areas-fill")) tameBasemap(map);
      // The density field goes above every basemap fill and line but under the trailing block
      // of labels, so place names stay readable on it. (Styles can interleave an early symbol
      // layer among the fills; "first symbol" would bury the field under water and landuse.)
      const styleLayers = map.getStyle()?.layers ?? [];
      let lastGeometry = -1;
      styleLayers.forEach((l, i) => {
        if (l.type !== "symbol") lastGeometry = i;
      });
      const labelsFrom = styleLayers[lastGeometry + 1]?.id;
      ensureLayers(map);
      layer.installStyleLayers(map, labelsFrom ?? "areas-fill");
      if (!map.getLayer(layer.id)) map.addLayer(layer, "highlight-glow");
      readyRef.current = true;
      syncData(map);
    });
    map.on("load", () => void loadWifi());
    map.on("error", (e) => {
      const msg = (e as { error?: { message?: string } }).error?.message ?? "";
      if (/glyph|font|sprite/i.test(msg)) return; // cosmetic with a blank style
      // If the basemap style itself cannot be fetched (offline, bad URL), fall back to a
      // blank style so `load` still fires and cameras render.
      if (!readyRef.current && !styleFallbackDone && !map.isStyleLoaded()) {
        styleFallbackDone = true;
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
      if (layer.handleClick(e.point.x, e.point.y, coarse.matches)) return;
      const layers = WIFI_CLICKABLE.filter((id) => map.getLayer(id));
      const hit = layers.length ? map.queryRenderedFeatures(e.point, { layers })[0] : undefined;
      if (!hit) {
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
        const over = layer.hoverTest(x, y) || (wifiLayers.length > 0 && map.queryRenderedFeatures([x, y], { layers: wifiLayers }).length > 0);
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
  }, [view !== null, styleUrl !== undefined]);

  // Basemap style changes from Settings.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || styleUrl === undefined || styleUrl === styleRef.current) return;
    styleRef.current = styleUrl;
    readyRef.current = false;
    map.setStyle(styleUrl ? styleUrl : BLANK_STYLE, { diff: false });
  }, [styleUrl]);

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
  }, [sightings, filters, zoom, highlighted, selection, alertState, drawPoints]);

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

  // Fly requests.
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !fly) return;
    if (fly.bbox) {
      map.fitBounds(
        [
          [fly.bbox.west, fly.bbox.south],
          [fly.bbox.east, fly.bbox.north],
        ],
        { padding: 80, maxZoom: 16, duration: 900 },
      );
    } else if (fly.lat !== undefined && fly.lon !== undefined) {
      map.flyTo({ center: [fly.lon, fly.lat], zoom: fly.zoom ?? Math.max(map.getZoom(), 14), duration: 900 });
    }
  }, [fly]);

  return (
    <>
      <div ref={containerRef} className={`map mode-${mode}`} />
      {styleUrl === "" && (
        <div className="no-style">
          <div className="callout info">
            <strong>No basemap configured.</strong> Cameras still show on this blank background.
            Open Settings and paste a MapLibre style URL (for example an OpenFreeMap style such as{" "}
            <code>https://tiles.openfreemap.org/styles/dark</code>).
          </div>
        </div>
      )}
    </>
  );
}
