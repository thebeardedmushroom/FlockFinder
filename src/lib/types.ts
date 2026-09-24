// TypeScript mirrors of the Rust structs crossing the Tauri IPC boundary.
// Field names are snake_case because that is how serde emits them.

export type Category = "flock" | "alpr" | "unknown";

export interface Camera {
  osm_type: string;
  osm_id: number;
  lat: number;
  lon: number;
  category: Category;
  tags: Record<string, string>;
  first_seen: number;
  last_seen: number;
  stale_since: number | null;
}

export const cameraKey = (c: Pick<Camera, "osm_type" | "osm_id">): string =>
  `${c.osm_type}/${c.osm_id}`;

export interface BBox {
  south: number;
  west: number;
  north: number;
  east: number;
}

export interface ViewState {
  lat: number;
  lon: number;
  zoom: number;
}

export interface Settings {
  /** "snapshot": the published daily snapshot, falling back to Overpass; "overpass": always query. */
  sync_source: "snapshot" | "overpass";
  /** Snapshot manifest URL; empty means the one built into the app. */
  snapshot_url: string;
  overpass_endpoint: string;
  cache_ttl_days: number;
  style_url: string;
  refresh_interval_hours: number;
  osm_client_id: string;
  notifications_enabled: boolean;
  first_run_done: boolean;
}

export interface AppInfo {
  version: string;
  repo_url: string;
  user_agent: string;
  db_path: string;
  first_run: boolean;
  fixture_mode: boolean;
  disclaimer: string;
  osm_redirect_uri: string;
  display_tags: string[];
}

/** State of the worldwide camera sync (`get_sync_status`, `sync:status` events). */
export interface SyncStatus {
  running: boolean;
  phase: "downloading" | "parsing" | "saving" | null;
  /** Bytes received so far while downloading (decompressed). */
  bytes: number;
  last_ok_at: number | null;
  last_attempt_at: number | null;
  last_outcome: "ok" | "error" | "offline" | null;
  last_error: string | null;
  last_elements: number | null;
  last_bytes: number | null;
  last_duration_secs: number | null;
  /** Cameras counted (not stale) and stale ones kept hollow. */
  cameras: number;
  stale_cameras: number;
  interval_days: number;
  next_due_at: number;
  fixture_mode: boolean;
  /** Where syncs come from under the current settings. */
  source: "snapshot" | "overpass";
  /** Where the stored cameras came from, and the OSM database time they reflect (ISO 8601). */
  data_source: string | null;
  data_as_of: string | null;
}

export interface SyncOutcome {
  elements: number;
  skipped: number;
  upserted: number;
  marked_stale: number;
  purged: number;
  bytes: number;
  source: string;
  /** Nothing newer than the stored data was found. */
  unchanged: boolean;
}

export interface CacheStats {
  cameras: number;
  stale_cameras: number;
  cells: number;
  oldest_fetch: number | null;
  newest_fetch: number | null;
}

export interface GeocodeResult {
  display_name: string;
  lat: number;
  lon: number;
  bbox: BBox | null;
  osm_type: string | null;
  osm_id: number | null;
}

export interface LandCheck {
  checked: boolean;
  on_land: boolean;
  place: string | null;
}

export type SubmissionCategory = "flock" | "alpr" | "unsure";
export type Mount = "pole" | "mast" | "building" | "other";

export interface Submission {
  id: number;
  lat: number;
  lon: number;
  category: SubmissionCategory;
  direction: number | null;
  mount: Mount | null;
  operator: string | null;
  notes: string | null;
  status: "local" | "uploaded";
  osm_element_id: number | null;
  created_at: number;
  updated_at: number;
}

export interface SubmissionInput {
  lat: number;
  lon: number;
  category: SubmissionCategory;
  direction: number | null;
  mount: Mount | null;
  operator: string | null;
  notes: string | null;
}

export interface Proximity {
  submissions: { submission: Submission; distance_m: number }[];
  cameras: { camera: Camera; distance_m: number }[];
}

export interface WatchArea {
  id: number;
  name: string;
  lat: number;
  lon: number;
  radius_m: number;
  created_at: number;
  last_checked: number | null;
}

export interface Route {
  id: number;
  name: string;
  geojson: string;
  corridor_m: number;
  created_at: number;
  last_checked: number | null;
}

export interface AlertEvent {
  id: number;
  target_type: "area" | "route";
  target_id: number;
  osm_type: string;
  osm_id: number;
  event: "baseline" | "added" | "removed";
  occurred_at: number;
  notified: boolean;
}

export interface AreaSummary {
  area: WatchArea;
  count: number;
  camera_keys: string[];
  baseline_recorded: boolean;
}

export interface RouteSummary {
  route: Route;
  count: number;
  camera_keys: string[];
  baseline_recorded: boolean;
}

export interface AlertState {
  areas: AreaSummary[];
  routes: RouteSummary[];
  last_refresh: number | null;
  last_skipped_offline: number | null;
  unseen_added: number;
  refresh_running: boolean;
  disclaimer: string;
}

export type Key = [string, number];

export interface TargetOutcome {
  target_type: "area" | "route";
  target_id: number;
  name: string;
  baseline: boolean;
  added: Key[];
  removed: Key[];
  count: number;
  notified: boolean;
}

export interface RefreshOutcome {
  started_at: number;
  finished_at: number;
  offline: boolean;
  requests: number;
  cells_fetched: number;
  targets: TargetOutcome[];
}

export interface TargetCreated<T> {
  target: T;
  count: number;
  baseline_pending: boolean;
  offline: boolean;
}

export interface RouteCamera {
  camera: Camera;
  distance_m: number;
  along_m: number;
  segment: number;
}

export interface RouteReport {
  route: Route;
  disclaimer: string;
  generated_at: number;
  length_m: number;
  cameras: RouteCamera[];
}

export interface GpxImport {
  name: string;
  points: [number, number][];
  length_m: number;
  path: string;
}

export interface OsmAuthStatus {
  configured: boolean;
  signed_in: boolean;
  client_id: string;
  pending: boolean;
  keychain_error: string | null;
}

export interface UploadResult {
  node_id: number;
  changeset_id: number;
}

export interface AuthOutcome {
  ok: boolean;
  message: string;
}

export interface FixtureLoad {
  stats: { requests: number; cells_fetched: number; elements: number; skipped: number };
  bbox: BBox;
}

export interface AppError {
  kind:
    | "db"
    | "offline"
    | "http"
    | "rate_limited"
    | "cancelled"
    | "parse"
    | "invalid"
    | "auth_required"
    | "not_configured"
    | "other";
  message: string;
}

export function isAppError(e: unknown): e is AppError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as AppError).message === "string"
  );
}

export function errorMessage(e: unknown): string {
  if (isAppError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

// ---------------------------------------------------------------------------
// Wi-Fi fingerprint sightings (suspected Flock devices; heuristic, separate from OSM)
// ---------------------------------------------------------------------------

export interface WifiSighting {
  netid: string;
  lat: number;
  lon: number;
  oui: string;
  ssid: string | null;
  channel: number | null;
  encryption: string | null;
  first_seen: string | null;
  last_seen: string | null;
  city: string | null;
  region: string | null;
  country: string | null;
  road: string | null;
  postalcode: string | null;
  source: "upstream" | "wigle_import";
  imported_at: number;
}

export interface WifiDatasetStatus {
  total: number;
  upstream: number;
  imported: number;
  downloaded_at: number | null;
  upstream_generated: string | null;
  upstream_total: number | null;
  dataset_url: string;
  repo_url: string;
  policy_url: string;
  oui_count: number;
  retention_days: number;
}

export interface WifiIngestResult {
  inserted: number;
  stats: {
    parsed: number;
    skipped_invalid: number;
    skipped_old: number;
    skipped_unmatched: number;
    deduplicated: number;
  };
  upstream_generated: string | null;
}

export interface OuiEntry {
  oui: string;
  vendor_context: string;
  detection_protocol: string;
  source: string;
  notes: string;
}
