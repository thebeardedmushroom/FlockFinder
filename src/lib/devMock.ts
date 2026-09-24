// Development-only in-memory stand-in for the Rust backend, so the React UI can be
// exercised in a plain browser (`npm run dev`, then open http://localhost:14210/?mock=1).
// Cameras come from `dev-data/overpass_global.json` when present (a saved worldwide sync
// response; gitignored, ODbL data), otherwise from the bundled Overpass fixture. Query
// options: `data=fixture` forces the fixture; `sync=never|failed|syncing` starts in that
// sync state with an empty store. Never included in production builds.
import { encodePoints } from "./cameraData";
import { classify } from "./classify";
import { haversineM } from "./geo";
import type {
  AlertEvent,
  AlertState,
  AppInfo,
  BBox,
  Camera,
  Route,
  Settings,
  Submission,
  SubmissionInput,
  SyncStatus,
  WatchArea,
  WifiSighting,
} from "./types";
import { cameraKey } from "./types";

type Args = Record<string, unknown>;

const DISCLAIMER =
  "Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence of a camera.";
const FIXTURE_BBOX: BBox = { south: 39.6, west: -105.15, north: 39.85, east: -104.85 };
const FIXTURE_URL = "/src-tauri/fixtures/overpass_sample.json";
const GLOBAL_URL = "/dev-data/overpass_global.json";

const now = () => Math.floor(Date.now() / 1000);
const fail = (kind: string, message: string) => Promise.reject({ kind, message });

export function installDevMock(): void {
  const params = new URLSearchParams(location.search);
  const w = window as unknown as Record<string, unknown>;
  const listeners = new Map<string, number[]>();
  const emit = (event: string, payload: unknown) => {
    for (const h of listeners.get(event) ?? []) (w[`_${h}`] as ((e: unknown) => void) | undefined)?.({ event, id: h, payload });
  };

  const settings: Settings = {
    sync_source: "snapshot",
    snapshot_url: "",
    overpass_endpoint: "https://overpass-api.de/api/interpreter",
    cache_ttl_days: 7,
    style_url: "https://tiles.openfreemap.org/styles/dark",
    refresh_interval_hours: 24,
    osm_client_id: "",
    notifications_enabled: true,
    first_run_done: false,
  };
  let camerasPromise: Promise<Camera[]> | null = null;
  const loadCameras = (): Promise<Camera[]> => {
    const fetchJson = async (): Promise<{ json: { elements: Array<Record<string, unknown>> }; fixture: boolean }> => {
      if (params.get("data") !== "fixture") {
        const r = await fetch(GLOBAL_URL).catch(() => null);
        if (r?.ok && r.headers.get("content-type")?.includes("json")) return { json: await r.json(), fixture: false };
      }
      return { json: await (await fetch(FIXTURE_URL)).json(), fixture: true };
    };
    camerasPromise ??= fetchJson().then(({ json, fixture }) => {
      const t = now();
      const out: Camera[] = [];
      for (const el of json.elements) {
        const c = (el.type === "node" ? el : (el.center as Record<string, unknown> | undefined)) ?? {};
        const lat = Number(c.lat);
        const lon = Number(c.lon);
        if (!Number.isFinite(lat) || !Number.isFinite(lon)) continue;
        const tags = (el.tags as Record<string, string>) ?? {};
        out.push({
          osm_type: String(el.type),
          osm_id: Number(el.id),
          lat,
          lon,
          category: classify(tags),
          tags,
          first_seen: t,
          last_seen: t,
          stale_since: fixture && out.length % 40 === 7 ? t - 86400 : null, // a few hollow markers for the demo
        });
      }
      console.info(`[mock] ${out.length} cameras from ${fixture ? "the fixture" : GLOBAL_URL}`);
      return out;
    });
    return camerasPromise;
  };

  // Worldwide sync state. `sync=never|failed|syncing` starts with an empty store.
  const syncMode = params.get("sync") ?? "ok";
  let synced = syncMode === "ok";
  const syncStatus: SyncStatus = {
    running: syncMode === "syncing",
    phase: syncMode === "syncing" ? "downloading" : null,
    bytes: syncMode === "syncing" ? 12_400_000 : 0,
    last_ok_at: synced ? now() - 3 * 3600 : null,
    last_attempt_at: syncMode === "never" || syncMode === "syncing" ? null : now() - 3 * 3600,
    last_outcome: syncMode === "failed" ? "error" : synced ? "ok" : null,
    last_error: syncMode === "failed" ? "HTTP 504 from Overpass: Gateway Timeout" : null,
    last_elements: synced ? 150_947 : null,
    last_bytes: synced ? 56_846_942 : null,
    last_duration_secs: synced ? 214 : null,
    cameras: 0,
    stale_cameras: 0,
    interval_days: 7,
    next_due_at: now() + (syncMode === "failed" ? 3600 : 86400),
    fixture_mode: false,
    source: "snapshot",
    data_source: synced ? "snapshot" : null,
    data_as_of: synced ? new Date((now() - 5 * 3600) * 1000).toISOString().replace(/.d+Z$/, "Z") : null,
  };
  let pointsBuffer: ArrayBuffer | null = null;
  const cameraPoints = async (): Promise<ArrayBuffer> => {
    const cams = synced ? await loadCameras() : [];
    syncStatus.cameras = cams.filter((c) => !c.stale_since).length;
    syncStatus.stale_cameras = cams.length - syncStatus.cameras;
    if (!synced) return encodePoints([]);
    pointsBuffer ??= encodePoints(
      cams.map((c) => ({
        osm_type: c.osm_type,
        osm_id: c.osm_id,
        lat: c.lat,
        lon: c.lon,
        category: c.category,
        stale: c.stale_since !== null,
        operator: c.tags.operator ?? null,
        direction: c.tags.direction ?? c.tags["camera:direction"] ?? null,
      })),
    );
    return pointsBuffer;
  };

  const submissions: Submission[] = [];
  // Wi-Fi fingerprint sightings: empty until "Download dataset" is pressed in the mock.
  let wifiSightings: WifiSighting[] = [];
  let wifiDownloadedAt: number | null = null;
  const fakeSightings = (): WifiSighting[] => {
    const out: WifiSighting[] = [];
    const ouis = ["70:C9:4E", "E0:4F:43", "08:3A:88", "74:4C:A1", "D0:39:57"];
    for (let i = 0; i < 80; i++) {
      const oui = ouis[i % ouis.length];
      const hex = (n: number) => n.toString(16).toUpperCase().padStart(2, "0");
      out.push({
        netid: `${oui}:${hex(i)}:${hex(i * 7)}:${hex(i * 13)}`,
        lat: 39.62 + ((i * 37) % 100) / 400,
        lon: -105.12 + ((i * 53) % 100) / 400,
        oui,
        ssid: i % 9 === 0 ? "Flock" : i % 17 === 0 ? `Flock-${hex(i)}A2B3` : null,
        channel: 1 + (i % 11),
        encryption: i % 3 === 0 ? "wpa2" : "unknown",
        first_seen: "2026-06-01T12:00:00.000Z",
        last_seen: "2026-08-20T12:00:00.000Z",
        city: "Denver",
        region: "CO",
        country: "US",
        road: i % 2 === 0 ? "W Colfax Ave" : "S Federal Blvd",
        postalcode: "80204",
        source: i % 10 === 0 ? "wigle_import" : "upstream",
        imported_at: now(),
      });
    }
    return out;
  };
  const areas: WatchArea[] = [];
  const routes: Route[] = [];
  const events: AlertEvent[] = [];
  let nextId = 1;
  let seenAt = 0;
  let lastRefresh: number | null = null;

  const inBox = (c: { lat: number; lon: number }, b: BBox) =>
    c.lat >= b.south && c.lat <= b.north && c.lon >= b.west && c.lon <= b.east;

  const routePoints = (r: Route): [number, number][] =>
    (JSON.parse(r.geojson).coordinates as [number, number][]).map(([lon, lat]) => [lat, lon]);

  const distToSegment = (p: [number, number], a: [number, number], b: [number, number]) => {
    const k = 111_320 * Math.cos((p[0] * Math.PI) / 180);
    const px = (p[1] - a[1]) * k;
    const py = (p[0] - a[0]) * 111_320;
    const bx = (b[1] - a[1]) * k;
    const by = (b[0] - a[0]) * 111_320;
    const len2 = bx * bx + by * by || 1;
    const t = Math.max(0, Math.min(1, (px * bx + py * by) / len2));
    return { d: Math.hypot(px - t * bx, py - t * by), t };
  };

  const camerasAlong = async (r: Route) => {
    const pts = routePoints(r);
    const cams = await loadCameras();
    const out: { camera: Camera; distance_m: number; along_m: number; segment: number }[] = [];
    for (const c of cams) {
      if (c.stale_since) continue;
      let best: { d: number; along: number; seg: number } | null = null;
      let cum = 0;
      for (let i = 1; i < pts.length; i++) {
        const { d, t } = distToSegment([c.lat, c.lon], pts[i - 1], pts[i]);
        const segLen = haversineM(pts[i - 1][0], pts[i - 1][1], pts[i][0], pts[i][1]);
        if (!best || d < best.d) best = { d, along: cum + t * segLen, seg: i - 1 };
        cum += segLen;
      }
      if (best && best.d <= r.corridor_m) out.push({ camera: c, distance_m: best.d, along_m: best.along, segment: best.seg });
    }
    return out.sort((a, b) => a.along_m - b.along_m);
  };

  const camerasIn = async (a: WatchArea) => {
    const cams = await loadCameras();
    return cams
      .filter((c) => !c.stale_since)
      .map((c) => ({ camera: c, distance_m: haversineM(a.lat, a.lon, c.lat, c.lon) }))
      .filter((x) => x.distance_m <= a.radius_m)
      .sort((x, y) => x.distance_m - y.distance_m);
  };

  const baseline = (type: "area" | "route", id: number, cams: Camera[]) => {
    const t = now();
    for (const c of cams) {
      events.push({ id: nextId++, target_type: type, target_id: id, osm_type: c.osm_type, osm_id: c.osm_id, event: "baseline", occurred_at: t, notified: false });
    }
  };

  const tagsFor = (s: Submission): [string, string][] => {
    const tags: [string, string][] = [
      ["man_made", "surveillance"],
      ["surveillance", "public"],
      ["surveillance:type", "ALPR"],
      ["surveillance:zone", "traffic"],
      ["camera:type", "fixed"],
    ];
    if (s.category === "flock") tags.push(["brand", "Flock Safety"], ["manufacturer", "Flock Safety"]);
    if (s.mount === "pole" || s.mount === "mast") tags.push(["camera:mount", s.mount]);
    if (s.mount === "building") tags.push(["camera:mount", "wall"]);
    if (s.direction !== null) tags.push(["direction", String(s.direction)]);
    if (s.operator) tags.push(["operator", s.operator]);
    return tags;
  };

  const handlers: Record<string, (a: Args) => Promise<unknown>> = {
    "plugin:event|listen": async (a) => {
      const event = String(a.event);
      const handler = Number(a.handler);
      listeners.set(event, [...(listeners.get(event) ?? []), handler]);
      return handler;
    },
    "plugin:event|unlisten": async () => undefined,
    get_app_info: async (): Promise<AppInfo> => ({
      version: "0.1.0",
      repo_url: "https://github.com/OWNER/flockfinder",
      user_agent: "FlockFinder/0.1.0 (+https://github.com/OWNER/flockfinder)",
      db_path: "(mock backend — nothing is persisted)",
      first_run: !settings.first_run_done,
      fixture_mode: true,
      disclaimer: DISCLAIMER,
      osm_redirect_uri: "flockfinder://oauth/callback",
      display_tags: ["direction", "operator", "brand", "manufacturer", "surveillance:zone", "camera:mount", "start_date", "ref"],
    }),
    get_settings: async () => ({ ...settings }),
    save_settings: async (a) => Object.assign(settings, a.settings as Settings),
    mark_first_run_done: async () => void (settings.first_run_done = true),
    get_initial_view: async () => ({ lat: 39.74, lon: -104.99, zoom: 12 }),
    save_view: async () => undefined,
    get_camera_points: cameraPoints,
    get_sync_status: async () => {
      await cameraPoints();
      return { ...syncStatus };
    },
    sync_now: async () => {
      if (syncStatus.running) return fail("other", "a camera sync is already running");
      syncStatus.running = true;
      for (const [phase, bytes] of [["downloading", 2_100_000], ["downloading", 3_900_000], ["parsing", 56_846_942], ["saving", 3_900_000]] as const) {
        syncStatus.phase = phase;
        syncStatus.bytes = bytes;
        emit("sync:status", { ...syncStatus });
        await new Promise((r) => setTimeout(r, 450));
      }
      synced = true;
      Object.assign(syncStatus, {
        running: false,
        phase: null,
        bytes: 0,
        last_ok_at: now(),
        last_attempt_at: now(),
        last_outcome: "ok",
        last_error: null,
        next_due_at: now() + 86400,
        data_source: "snapshot",
        data_as_of: new Date(now() * 1000).toISOString().replace(/.d+Z$/, "Z"),
      });
      await cameraPoints();
      emit("sync:status", { ...syncStatus });
      emit("cameras:changed", null);
      return { elements: syncStatus.cameras, skipped: 0, upserted: syncStatus.cameras, marked_stale: 0, purged: 0, bytes: 3_900_000, source: "snapshot", unchanged: false };
    },
    get_cached_cameras: async (a) => (await loadCameras()).filter((c) => inBox(c, a.bbox as BBox)),
    cameras_near: async (a) => {
      const lat = Number(a.lat);
      const lon = Number(a.lon);
      const dLat = Number(a.radiusM) / 111_320;
      const dLon = dLat / Math.cos((lat * Math.PI) / 180);
      return (await loadCameras()).filter((c) => inBox(c, { south: lat - dLat, north: lat + dLat, west: lon - dLon, east: lon + dLon }));
    },
    get_cache_stats: async () => {
      const cams = await loadCameras();
      return { cameras: cams.length, stale_cameras: cams.filter((c) => c.stale_since).length, cells: 30, oldest_fetch: now() - 3600, newest_fetch: now() };
    },
    clear_cache: async () => undefined,
    load_fixture: async () => ({ stats: { requests: 0, cells_fetched: 30, elements: (await loadCameras()).length, skipped: 0 }, bbox: FIXTURE_BBOX }),
    geocode: async (a) => {
      const q = String(a.query).toLowerCase();
      if (q.includes("denver")) return [{ display_name: "Denver, Colorado, United States", lat: 39.7392, lon: -104.9849, bbox: { south: 39.614, west: -105.11, north: 39.914, east: -104.6 }, osm_type: "relation", osm_id: 1411339 }];
      return [];
    },
    check_location: async () => ({ checked: true, on_land: true, place: "Denver, Colorado (mock)" }),
    list_submissions: async () => [...submissions],
    create_submission: async (a) => {
      const i = a.input as SubmissionInput;
      const s: Submission = { id: nextId++, ...i, status: "local", osm_element_id: null, created_at: now(), updated_at: now() };
      submissions.unshift(s);
      return s;
    },
    update_submission: async (a) => {
      const s = submissions.find((x) => x.id === a.id);
      if (!s) return fail("invalid", "submission not found");
      Object.assign(s, a.input as SubmissionInput, { updated_at: now() });
      return s;
    },
    delete_submission: async (a) => {
      const i = submissions.findIndex((x) => x.id === a.id);
      if (i >= 0) submissions.splice(i, 1);
      return i >= 0;
    },
    check_submission_proximity: async (a) => {
      const lat = a.lat as number;
      const lon = a.lon as number;
      const cams = await loadCameras();
      return {
        submissions: submissions.filter((s) => s.id !== a.excludeId).map((s) => ({ submission: s, distance_m: haversineM(lat, lon, s.lat, s.lon) })).filter((x) => x.distance_m <= 15),
        cameras: cams.map((c) => ({ camera: c, distance_m: haversineM(lat, lon, c.lat, c.lon) })).filter((x) => x.distance_m <= 15),
      };
    },
    export_josm: async () => "C:\\mock\\flockfinder-submissions.osm",
    create_watch_area: async (a) => {
      const area: WatchArea = { id: nextId++, name: String(a.name), lat: a.lat as number, lon: a.lon as number, radius_m: a.radiusM as number, created_at: now(), last_checked: now() };
      if (area.radius_m < 100 || area.radius_m > 10000) return fail("invalid", "radius must be between 100 m and 10000 m");
      areas.push(area);
      const cams = await camerasIn(area);
      baseline("area", area.id, cams.map((c) => c.camera));
      return { target: area, count: cams.length, baseline_pending: false, offline: false };
    },
    list_watch_areas: async () => [...areas],
    rename_watch_area: async (a) => {
      const w = areas.find((x) => x.id === a.id);
      if (w) w.name = String(a.name);
    },
    delete_watch_area: async (a) => {
      const i = areas.findIndex((x) => x.id === a.id);
      if (i >= 0) areas.splice(i, 1);
    },
    create_route: async (a) => {
      const pts = a.points as [number, number][];
      const route: Route = { id: nextId++, name: String(a.name), geojson: JSON.stringify({ type: "LineString", coordinates: pts.map(([lat, lon]) => [lon, lat]) }), corridor_m: a.corridorM as number, created_at: now(), last_checked: now() };
      routes.push(route);
      const cams = await camerasAlong(route);
      baseline("route", route.id, cams.map((c) => c.camera));
      return { target: route, count: cams.length, baseline_pending: false, offline: false };
    },
    list_routes: async () => [...routes],
    delete_route: async (a) => {
      const i = routes.findIndex((x) => x.id === a.id);
      if (i >= 0) routes.splice(i, 1);
    },
    import_gpx: async () => null,
    get_alert_state: async (): Promise<AlertState> => ({
      areas: await Promise.all(areas.map(async (area) => {
        const cams = await camerasIn(area);
        return { area, count: cams.length, camera_keys: cams.map((c) => cameraKey(c.camera)), baseline_recorded: true };
      })),
      routes: await Promise.all(routes.map(async (route) => {
        const cams = await camerasAlong(route);
        return { route, count: cams.length, camera_keys: cams.map((c) => cameraKey(c.camera)), baseline_recorded: true };
      })),
      last_refresh: lastRefresh,
      last_skipped_offline: null,
      unseen_added: events.filter((e) => e.event === "added" && e.occurred_at > seenAt).length,
      refresh_running: false,
      disclaimer: DISCLAIMER,
    }),
    get_alert_history: async (a) => events.filter((e) => e.target_type === a.targetType && e.target_id === a.targetId).reverse(),
    get_target_cameras: async (a) => {
      if (a.targetType === "area") {
        const area = areas.find((x) => x.id === a.targetId);
        return area ? (await camerasIn(area)).map((c) => c.camera) : [];
      }
      const route = routes.find((x) => x.id === a.targetId);
      return route ? (await camerasAlong(route)).map((c) => c.camera) : [];
    },
    get_cameras_by_keys: async (a) => {
      const keys = new Set(a.keys as string[]);
      return (await loadCameras()).filter((c) => keys.has(cameraKey(c)));
    },
    acknowledge_alerts: async () => void (seenAt = now()),
    run_alert_refresh: async () => {
      lastRefresh = now();
      return { started_at: lastRefresh, finished_at: lastRefresh, offline: false, requests: 0, cells_fetched: 0, targets: [] };
    },
    get_route_report: async (a) => {
      const route = routes.find((x) => x.id === a.routeId);
      if (!route) return fail("invalid", "route not found");
      const pts = routePoints(route);
      let length = 0;
      for (let i = 1; i < pts.length; i++) length += haversineM(pts[i - 1][0], pts[i - 1][1], pts[i][0], pts[i][1]);
      return { route, disclaimer: DISCLAIMER, generated_at: now(), length_m: length, cameras: await camerasAlong(route) };
    },
    export_route_report: async (a) => `C:\\mock\\route-report.${a.format}`,
    osm_auth_status: async () => ({ configured: settings.osm_client_id !== "", signed_in: false, client_id: settings.osm_client_id, pending: false, keychain_error: null }),
    osm_sign_in: async () => fail("not_configured", "OSM sign-in is not available in the mock backend"),
    osm_complete_auth: async () => ({ ok: false, message: "mock backend" }),
    osm_sign_out: async () => undefined,
    preview_submission_tags: async (a) => {
      const s = submissions.find((x) => x.id === a.id);
      return s ? tagsFor(s) : fail("invalid", "submission not found");
    },
    osm_upload_submission: async (a) => (a.confirmed ? fail("auth_required", "OSM sign-in required") : fail("invalid", "upload requires confirmation")),
    wifi_dataset_status: async () => ({
      total: wifiSightings.length,
      upstream: wifiSightings.filter((s) => s.source === "upstream").length,
      imported: wifiSightings.filter((s) => s.source === "wigle_import").length,
      downloaded_at: wifiDownloadedAt,
      upstream_generated: wifiDownloadedAt ? "2026-09-10T10:14:50Z" : null,
      upstream_total: wifiDownloadedAt ? 144142 : null,
      dataset_url: "https://raw.githubusercontent.com/simeononsecurity/flock-finder/main/data/flock_cameras.csv",
      repo_url: "https://github.com/simeononsecurity/flock-finder",
      policy_url: "https://github.com/simeononsecurity/flock-finder/blob/main/docs/DATA_POLICY.md",
      oui_count: 31,
      retention_days: 730,
    }),
    wifi_download_dataset: async () => {
      await new Promise((r) => setTimeout(r, 600));
      wifiSightings = fakeSightings();
      wifiDownloadedAt = now();
      return {
        inserted: wifiSightings.length,
        stats: { parsed: wifiSightings.length, skipped_invalid: 2, skipped_old: 5, skipped_unmatched: 0, deduplicated: 1 },
        upstream_generated: "2026-09-10T10:14:50Z",
      };
    },
    wifi_import_wigle: async () => null,
    wifi_clear: async (a) => {
      const before = wifiSightings.length;
      wifiSightings = a.source ? wifiSightings.filter((s) => s.source !== a.source) : [];
      if (a.source !== "wigle_import") wifiDownloadedAt = null;
      return before - wifiSightings.length;
    },
    get_wifi_sightings: async (a) => wifiSightings.filter((s) => inBox(s, a.bbox as BBox)),
    wifi_oui_list: async () => [{ oui: "70:C9:4E", vendor_context: "Flock Safety infrastructure", detection_protocol: "WiFi 2.4 GHz", source: "@NitekryDPaul", notes: "" }],
    open_external: async (a) => console.log("[mock] open_external", a.url),
    notify: async (a) => (console.log("[mock] notify", a.title, a.body), false),
    frontend_log: async () => undefined,
  };

  let cbId = 0;
  w.__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args: Args = {}) => {
      const h = handlers[cmd];
      if (!h) return fail("other", `mock backend: unknown command ${cmd}`);
      return h(args);
    },
    transformCallback: (cb: (r: unknown) => void) => {
      const id = ++cbId;
      w[`_${id}`] = cb;
      return id;
    },
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  };
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => undefined };
  console.info("[mock] Flock Finder dev mock backend installed");
}
