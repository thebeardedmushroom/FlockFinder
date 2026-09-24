// Typed wrappers around Tauri commands. Every network call happens in Rust.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AlertEvent,
  AlertState,
  AppInfo,
  AuthOutcome,
  BBox,
  CacheStats,
  Camera,
  FixtureLoad,
  GeocodeResult,
  GpxImport,
  LandCheck,
  OsmAuthStatus,
  OuiEntry,
  WifiDatasetStatus,
  WifiIngestResult,
  WifiSighting,
  Proximity,
  RefreshOutcome,
  Route,
  RouteReport,
  Settings,
  Submission,
  SubmissionInput,
  SyncOutcome,
  SyncStatus,
  TargetCreated,
  UploadResult,
  ViewState,
  WatchArea,
} from "./types";

export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export const api = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<Settings>("save_settings", { settings }),
  markFirstRunDone: () => invoke<void>("mark_first_run_done"),
  getInitialView: () => invoke<ViewState>("get_initial_view"),
  saveView: (view: ViewState) => invoke<void>("save_view", { view }),

  /** Every camera in the local store as a binary snapshot (see lib/cameraData.ts). */
  getCameraPoints: () => invoke<ArrayBuffer>("get_camera_points"),
  getSyncStatus: () => invoke<SyncStatus>("get_sync_status"),
  /** Run the worldwide sync now; progress arrives through onSyncStatus. */
  syncNow: () => invoke<SyncOutcome>("sync_now"),
  getCachedCameras: (bbox: BBox) => invoke<Camera[]>("get_cached_cameras", { bbox }),
  /** Cameras within `radiusM` of a point, from the local store (no network). */
  camerasNear: (lat: number, lon: number, radiusM: number) =>
    invoke<Camera[]>("cameras_near", { lat, lon, radiusM }),
  getCacheStats: () => invoke<CacheStats>("get_cache_stats"),
  clearCache: () => invoke<void>("clear_cache"),
  loadFixture: () => invoke<FixtureLoad>("load_fixture"),

  geocode: (query: string) => invoke<GeocodeResult[]>("geocode", { query }),
  checkLocation: (lat: number, lon: number) => invoke<LandCheck>("check_location", { lat, lon }),

  listSubmissions: () => invoke<Submission[]>("list_submissions"),
  createSubmission: (input: SubmissionInput) => invoke<Submission>("create_submission", { input }),
  updateSubmission: (id: number, input: SubmissionInput) =>
    invoke<Submission>("update_submission", { id, input }),
  deleteSubmission: (id: number) => invoke<boolean>("delete_submission", { id }),
  checkSubmissionProximity: (lat: number, lon: number, excludeId: number | null) =>
    invoke<Proximity>("check_submission_proximity", { lat, lon, excludeId }),
  exportJosm: (ids: number[]) => invoke<string | null>("export_josm", { ids }),

  createWatchArea: (name: string, lat: number, lon: number, radiusM: number) =>
    invoke<TargetCreated<WatchArea>>("create_watch_area", { name, lat, lon, radiusM }),
  listWatchAreas: () => invoke<WatchArea[]>("list_watch_areas"),
  renameWatchArea: (id: number, name: string) => invoke<void>("rename_watch_area", { id, name }),
  deleteWatchArea: (id: number) => invoke<void>("delete_watch_area", { id }),
  createRoute: (name: string, points: [number, number][], corridorM: number) =>
    invoke<TargetCreated<Route>>("create_route", { name, points, corridorM }),
  listRoutes: () => invoke<Route[]>("list_routes"),
  deleteRoute: (id: number) => invoke<void>("delete_route", { id }),
  importGpx: () => invoke<GpxImport | null>("import_gpx"),
  getAlertState: () => invoke<AlertState>("get_alert_state"),
  getAlertHistory: (targetType: "area" | "route", targetId: number) =>
    invoke<AlertEvent[]>("get_alert_history", { targetType, targetId }),
  getTargetCameras: (targetType: "area" | "route", targetId: number) =>
    invoke<Camera[]>("get_target_cameras", { targetType, targetId }),
  getCamerasByKeys: (keys: string[]) => invoke<Camera[]>("get_cameras_by_keys", { keys }),
  acknowledgeAlerts: () => invoke<void>("acknowledge_alerts"),
  runAlertRefresh: () => invoke<RefreshOutcome>("run_alert_refresh"),
  getRouteReport: (routeId: number) => invoke<RouteReport>("get_route_report", { routeId }),
  exportRouteReport: (routeId: number, format: "csv" | "geojson") =>
    invoke<string | null>("export_route_report", { routeId, format }),

  osmAuthStatus: () => invoke<OsmAuthStatus>("osm_auth_status"),
  osmSignIn: () => invoke<string>("osm_sign_in"),
  osmCompleteAuth: (url: string) => invoke<AuthOutcome>("osm_complete_auth", { url }),
  osmSignOut: () => invoke<void>("osm_sign_out"),
  previewSubmissionTags: (id: number) => invoke<[string, string][]>("preview_submission_tags", { id }),
  osmUploadSubmission: (id: number, comment: string, confirmed: boolean) =>
    invoke<UploadResult>("osm_upload_submission", { id, comment, confirmed }),

  openExternal: (url: string) => invoke<void>("open_external", { url }),
  notify: (title: string, body: string) => invoke<boolean>("notify", { title, body }),

  wifiDatasetStatus: () => invoke<WifiDatasetStatus>("wifi_dataset_status"),
  wifiDownloadDataset: () => invoke<WifiIngestResult>("wifi_download_dataset"),
  wifiImportWigle: () => invoke<WifiIngestResult | null>("wifi_import_wigle"),
  wifiClear: (source: "upstream" | "wigle_import" | null) => invoke<number>("wifi_clear", { source }),
  getWifiSightings: (bbox: BBox) => invoke<WifiSighting[]>("get_wifi_sightings", { bbox }),
  wifiOuiList: () => invoke<OuiEntry[]>("wifi_oui_list"),
};

export function onAlertsRefreshed(handler: (o: RefreshOutcome) => void): Promise<UnlistenFn> {
  return listen<RefreshOutcome>("alerts:refreshed", (e) => handler(e.payload));
}

export function onOsmAuth(handler: (o: AuthOutcome) => void): Promise<UnlistenFn> {
  return listen<AuthOutcome>("osm:auth", (e) => handler(e.payload));
}

export function onSyncStatus(handler: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>("sync:status", (e) => handler(e.payload));
}

/** The cameras table changed (a sync, an alert refresh, sample data): reload the snapshot. */
export function onCamerasChanged(handler: () => void): Promise<UnlistenFn> {
  return listen("cameras:changed", () => handler());
}
