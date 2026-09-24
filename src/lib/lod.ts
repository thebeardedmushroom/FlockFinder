/**
 * Level of detail for the camera layer: which representation each zoom level shows. Every
 * breakpoint and size constant for camera rendering lives here, not in the render code.
 *
 *   wide    z0–5    regional density field (hex cells, fill by count); no individual features
 *   mid     z6–10   cluster nodes, area proportional to count, labelled with the count
 *   near    z11–13  small clusters; isolated cameras drawn individually
 *   detail  z14+    individual cameras only, with direction cones
 *
 * A "level" is the integer zoom the aggregation was computed for (floor of the map zoom).
 */
export type Band = "wide" | "mid" | "near" | "detail";

export const WIDE_MAX_LEVEL = 5;
export const MID_MAX_LEVEL = 10;
export const NEAR_MAX_LEVEL = 13;
/** Deepest level the renderer distinguishes (the map's max zoom is 19). */
export const MAX_LEVEL = 20;
/** First level drawn as cluster nodes (the wide band draws hex cells instead). */
export const CLUSTER_MIN_LEVEL = WIDE_MAX_LEVEL + 1;
/** From this level every camera is its own node: the cluster index stops at NEAR_MAX_LEVEL. */
export const LEAF_LEVEL = NEAR_MAX_LEVEL + 1;

export function levelForZoom(zoom: number): number {
  return Math.max(0, Math.min(MAX_LEVEL, Math.floor(zoom + 1e-9)));
}

export function bandForLevel(level: number): Band {
  if (level <= WIDE_MAX_LEVEL) return "wide";
  if (level <= MID_MAX_LEVEL) return "mid";
  if (level <= NEAR_MAX_LEVEL) return "near";
  return "detail";
}

export const bandForZoom = (zoom: number): Band => bandForLevel(levelForZoom(zoom));

/** A density map without a scale is decoration: the legend shows wherever aggregates do. */
export const bandHasLegend = (band: Band): boolean => band === "wide" || band === "mid";

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/** Cluster radius, CSS px at the level's zoom (MapLibre zoom, 512 px tiles). */
export const CLUSTER_RADIUS_PX = 60;
/** Hex cell circumradius at the level's zoom, CSS px. */
export const HEX_RADIUS_PX = 22;

// ---------------------------------------------------------------------------
// Encoding: node area ∝ count, luminance ∝ log density
// ---------------------------------------------------------------------------

/** Radius of the largest cluster at a level; every other node is sized relative to it. */
export const NODE_MAX_RADIUS_PX = 34;
/** Below this radius a node would not be visible or clickable; smaller counts are clamped. */
export const NODE_MIN_RADIUS_PX = 5.5;
/** Individual camera core radius. */
export const POINT_RADIUS_PX = 4.5;
/** Halo quad radius as a multiple of the core radius. */
export const HALO_SCALE = 3.0;
/** Peak additive halo contribution; low so dense metros don't sum into a white blob. */
export const HALO_MAX_INTENSITY = 0.34;

/**
 * px per √camera at a level, from that level's largest cluster in the whole (filtered)
 * dataset. Global rather than per viewport, so sizes don't change while panning.
 */
export function levelScale(maxCount: number): number {
  return NODE_MAX_RADIUS_PX / Math.sqrt(Math.max(1, maxCount));
}

/** Radius with area proportional to count: r = scale·√count, clamped at the minimum. */
export function nodeRadius(count: number, scale: number): number {
  return Math.max(NODE_MIN_RADIUS_PX, scale * Math.sqrt(count));
}

/** Smallest count whose radius is above the clamp (the legend states it). */
export function minProportionalCount(scale: number): number {
  return Math.ceil((NODE_MIN_RADIUS_PX / scale) ** 2);
}

/** Position on the luminance ramp: log(count) / log(max), in [0, 1]. */
export function densityT(count: number, maxCount: number): number {
  if (maxCount <= 1 || count <= 1) return count >= maxCount ? 1 : 0;
  return Math.max(0, Math.min(1, Math.log(count) / Math.log(maxCount)));
}

// ---------------------------------------------------------------------------
// Motion
// ---------------------------------------------------------------------------

/**
 * Band and level transitions (split/merge, crossfades). Spec: complete within 300 ms of
 * the level change; the worker query before it takes a few ms, so the animation gets 250.
 */
export const TRANSITION_MS = 250;
/** With prefers-reduced-motion: crossfade in place, no travel. */
export const TRANSITION_REDUCED_MS = 120;
/** New nodes entering from a pan fade in over this long. */
export const FADE_IN_MS = 160;
/** Slow pulse on the densest clusters after the map settles; a few cycles, then still. */
export const PULSE_PERIOD_MS = 2600;
export const PULSE_CYCLES = 3;
export const PULSE_MIN_T = 0.8;
export const PULSE_MAX_NODES = 4;
/** One scanline sweep when the dataset first appears. */
export const SWEEP_MS = 1200;

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

/** Direction cones appear from this level (isolated cameras in the near band get them too). */
export const CONE_MIN_LEVEL = 13;
export const CONE_RADIUS_PX = 44;

export function coneScale(zoom: number): number {
  if (zoom <= 13) return 0.55;
  if (zoom <= 16) return 0.55 + ((zoom - 13) / 3) * 0.35;
  return Math.min(1.3, 0.9 + ((zoom - 16) / 3) * 0.4);
}

/** Cameras closer than this on screen are spread out (spiderfied) when clicked. */
export const SPIDER_PICK_PX = 9;
export const SPIDER_MIN_LEVEL = LEAF_LEVEL;

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

/** Queries cover the viewport plus this fraction on every side, so small pans need none. */
export const QUERY_PAD = 0.5;
export const COUNT_THROTTLE_MS = 180;
export const FILTER_DEBOUNCE_MS = 120;

// ---------------------------------------------------------------------------
// Wi-Fi sightings: a separate heuristic layer, still loaded per viewport (local table only)
// ---------------------------------------------------------------------------

export const WIFI_MIN_ZOOM = 11;
export const WIFI_CLUSTER_THRESHOLD = 50;

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

/** Cluster label: exact below 1,000, abbreviated above (1.2k, 12k, 1.2M). */
export function formatCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  if (n < 999_500) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}

const EXACT = new Intl.NumberFormat("en-US");

/** HUD readouts: always exact, grouped. (A shared formatter: toLocaleString builds one per call.) */
export function formatExact(n: number): string {
  return EXACT.format(n);
}
