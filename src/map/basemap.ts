/**
 * Transforms on basemap style JSON, applied before the style reaches the map (so there is no
 * flash of the original colours and nothing to undo on the next style swap):
 *
 * - `tameLayers` pushes a style into the background: every colour is desaturated toward a
 *   cool neutral and its contrast against the dark ground reduced, and labels are dimmed.
 *   Camera data is then the only saturated thing on screen, while roads and place names stay
 *   readable enough to orient by. The Midnight theme and custom style URLs use it.
 * - `recolorLayers` repaints an OpenMapTiles-schema style from a palette, layer by role
 *   (land, water, road tier, labels…). The authored themes use it.
 * - `latinLabelsAtWideZoom` is a performance fix every style gets.
 */
import type * as maplibregl from "maplibre-gl";
import { WIDE_MAX_LEVEL } from "../lib/lod";

/**
 * Below this zoom (the wide band), bilingual labels show only their Latin name. MapLibre draws
 * any multi-codepoint grapheme cluster (Mongolian, Tibetan, Devanagari, Thai… marks) itself on
 * the main thread, and at wide zoom a pan streams whole continents of such labels past at once:
 * that was 25–35 ms of glyph rasterising per batch. From the mid band on, labels are bilingual
 * again as the style intended.
 */
const LATIN_ONLY_BELOW_ZOOM = WIDE_MAX_LEVEL + 1;
const LATIN_NAME = ["coalesce", ["get", "name:latin"], ["get", "name_en"], ["get", "name"]];

function mentions(v: unknown, needle: string): boolean {
  if (typeof v === "string") return v === needle;
  if (Array.isArray(v)) return v.some((x) => mentions(x, needle));
  return false;
}

/** Share of the original chroma removed (1 = fully grey). */
const DESATURATE = 0.88;
/** Distance from the ground colour kept (1 = original contrast). */
const CONTRAST = 0.72;
/** Near-black the contrast is compressed toward. */
const GROUND = [0.035, 0.045, 0.06];
const COOL = [0.96, 1.0, 1.07];
const TEXT_OPACITY = 0.62;
const LINE_OPACITY = 0.72;
const ICON_OPACITY = 0.35;

const COLOR_PROPS: Record<string, string[]> = {
  background: ["background-color"],
  fill: ["fill-color", "fill-outline-color"],
  line: ["line-color"],
  symbol: ["text-color", "text-halo-color", "icon-color", "icon-halo-color"],
  circle: ["circle-color", "circle-stroke-color"],
  "fill-extrusion": ["fill-extrusion-color"],
};

const NAMED: Record<string, [number, number, number, number]> = {
  white: [1, 1, 1, 1],
  black: [0, 0, 0, 1],
  transparent: [0, 0, 0, 0],
};

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const k = (n: number) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [f(0), f(8), f(4)];
}

/** Parse the CSS colour forms map styles use (hex, rgb[a], hsl[a], a few names). */
export function parseCssColor(v: string): [number, number, number, number] | null {
  const s = v.trim().toLowerCase();
  if (NAMED[s]) return NAMED[s];
  let m = /^#([0-9a-f]{3,8})$/.exec(s);
  if (m) {
    const h = m[1];
    if (h.length === 3 || h.length === 4) {
      const c = [...h].map((x) => parseInt(x + x, 16) / 255);
      return [c[0], c[1], c[2], c[3] ?? 1];
    }
    if (h.length === 6 || h.length === 8) {
      const c = [0, 2, 4, 6].map((i) => (i < h.length ? parseInt(h.slice(i, i + 2), 16) / 255 : 1));
      return [c[0], c[1], c[2], c[3]];
    }
    return null;
  }
  m = /^(rgba?|hsla?)\(([^)]+)\)$/.exec(s);
  if (!m) return null;
  const parts = m[2].split(/[\s,/]+/).filter(Boolean);
  if (parts.length < 3) return null;
  const num = (p: string, scale: number) => (p.endsWith("%") ? parseFloat(p) / 100 : parseFloat(p) / scale);
  const alpha = parts[3] !== undefined ? num(parts[3], 1) : 1;
  if ([0, 1, 2].some((i) => Number.isNaN(parseFloat(parts[i])))) return null;
  if (m[1].startsWith("rgb")) return [num(parts[0], 255), num(parts[1], 255), num(parts[2], 255), alpha];
  const [r, g, b] = hslToRgb(parseFloat(parts[0]), num(parts[1], 100), num(parts[2], 100));
  return [r, g, b, alpha];
}

function mute(css: string): string | null {
  const c = parseCssColor(css);
  if (!c) return null;
  const [r, g, b, a] = c;
  const lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  const out = [r, g, b].map((v, i) => {
    const grey = v + (lum * COOL[i] - v) * DESATURATE;
    return Math.round(Math.max(0, Math.min(1, GROUND[i] + (grey - GROUND[i]) * CONTRAST)) * 255);
  });
  return `rgba(${out[0]}, ${out[1]}, ${out[2]}, ${Number(a.toFixed(3))})`;
}

/** Replace every colour literal inside a paint value, including inside expressions. */
function muteValue(v: unknown): unknown {
  if (typeof v === "string") return mute(v) ?? v;
  if (Array.isArray(v)) return v.map(muteValue);
  if (v && typeof v === "object") {
    const o: Record<string, unknown> = {};
    for (const [k, x] of Object.entries(v)) o[k] = muteValue(x);
    return o;
  }
  return v;
}

/**
 * Roads get their own treatment. Muting them like everything else left the dark style's
 * roads within a few levels of the ground colour, i.e. invisible. Instead each road line is
 * a flat cool grey set this far (in lightness) from the muted background, scaled by the
 * road's importance, so the network reads at a glance without competing with camera colours.
 */
const ROAD_CONTRAST = 0.3;
/** Casings sit under the road fill: a softer edge, so roads read as one line with an outline. */
const ROAD_CASING_SHARE = 0.5;

/** Importance of a road/rail line layer from its id (OpenMapTiles naming), or null if not a road. */
export function roadWeight(layerId: string, sourceLayer: string | undefined): number | null {
  if (sourceLayer !== "transportation") return null;
  const id = layerId.toLowerCase();
  // Rail dash overlays are drawn in the ground colour on purpose; leave them to the mute pass.
  if (id.includes("dash")) return null;
  if (id.includes("motorway")) return 1;
  if (/trunk|primary|major/.test(id)) return 0.8;
  if (/secondary|tertiary/.test(id)) return 0.65;
  if (/rail|path|pier|track|foot|cycle|steps/.test(id)) return 0.35;
  return 0.5;
}

function roadColor(background: [number, number, number, number] | null, weight: number, casing: boolean): string {
  const bg = background ? 0.2126 * background[0] + 0.7152 * background[1] + 0.0722 * background[2] : GROUND[1];
  const delta = ROAD_CONTRAST * weight * (casing ? ROAD_CASING_SHARE : 1);
  const l = bg < 0.5 ? bg + delta : bg - delta;
  const [r, g, b] = COOL.map((c) => Math.round(Math.max(0, Math.min(1, l * c)) * 255));
  return `rgba(${r}, ${g}, ${b}, 1)`;
}

/** A style layer as JSON, loosely typed: transforms read and write paint/layout by name. */
type StyleLayer = {
  id: string;
  type: string;
  "source-layer"?: string;
  paint?: Record<string, unknown>;
  layout?: Record<string, unknown>;
};

function paintOf(layer: StyleLayer): Record<string, unknown> {
  layer.paint ??= {};
  return layer.paint;
}

function setIfPlain(paint: Record<string, unknown>, prop: string, value: number): void {
  const cur = paint[prop];
  // Zoom-dependent expressions must stay top-level, so only plain numbers are replaced.
  if (cur === undefined || typeof cur === "number") paint[prop] = typeof cur === "number" ? Math.min(cur, value) : value;
}

/** Mute every layer of a style in place (see the module comment). */
export function tameLayers(layersIn: maplibregl.LayerSpecification[]): void {
  const layers = layersIn as unknown as StyleLayer[];
  const bgValue = layers.find((l) => l.type === "background")?.paint?.["background-color"];
  const bgMuted = typeof bgValue === "string" ? mute(bgValue) : null;
  const background = bgMuted ? parseCssColor(bgMuted) : null;
  for (const layer of layers) {
    const paint = paintOf(layer);
    const weight = layer.type === "line" ? roadWeight(layer.id, layer["source-layer"]) : null;
    if (weight !== null) {
      paint["line-color"] = roadColor(background, weight, layer.id.includes("casing"));
      if (typeof paint["line-opacity"] !== "object") paint["line-opacity"] = 1;
      continue;
    }
    for (const prop of COLOR_PROPS[layer.type] ?? []) {
      if (paint[prop] !== undefined) paint[prop] = muteValue(paint[prop]);
    }
    if (layer.type === "symbol") {
      setIfPlain(paint, "text-opacity", TEXT_OPACITY);
      setIfPlain(paint, "icon-opacity", ICON_OPACITY);
    } else if (layer.type === "line") {
      setIfPlain(paint, "line-opacity", LINE_OPACITY);
    } else if (layer.type === "raster") {
      paint["raster-saturation"] = -0.85;
      paint["raster-contrast"] = -0.25;
    }
  }
}

/** Show only Latin names in the wide band (see LATIN_ONLY_BELOW_ZOOM). */
export function latinLabelsAtWideZoom(layers: maplibregl.LayerSpecification[]): void {
  for (const layer of layers as unknown as StyleLayer[]) {
    if (layer.type !== "symbol") continue;
    const field = layer.layout?.["text-field"];
    if (mentions(field, "name:nonlatin") && !mentions(field, "zoom")) {
      layer.layout!["text-field"] = ["step", ["zoom"], LATIN_NAME, LATIN_ONLY_BELOW_ZOOM, field];
    }
  }
}

// ---------------------------------------------------------------------------
// Recolouring by role
// ---------------------------------------------------------------------------

/**
 * Every colour a themed basemap defines. Keys left out keep the base style's colour, so a
 * theme built on a provider style can adjust just a few roles.
 */
export interface BasemapPalette {
  /** Land with nothing else on it. */
  background: string;
  /** Built-up land: residential, commercial, industrial… */
  landuse: string;
  /** Parks, woods, grass. */
  green: string;
  ice: string;
  water: string;
  /** Rivers and streams drawn as lines. */
  waterway: string;
  building: string;
  buildingOutline: string;
  /** Runways, taxiways, aprons. */
  aeroway: string;
  motorway: string;
  /** Trunk and primary roads. */
  primary: string;
  /** Secondary and tertiary roads. */
  secondary: string;
  /** Residential, service and every other minor road. */
  minor: string;
  path: string;
  rail: string;
  /** Road outlines (drawn under the road fill). */
  casing: string;
  boundary: string;
  /** Place, POI and airport labels. */
  label: string;
  roadLabel: string;
  waterLabel: string;
  /** Halo behind every label. */
  labelHalo: string;
}

export type Role =
  | "background"
  | "landuse"
  | "green"
  | "ice"
  | "water"
  | "waterway"
  | "building"
  | "aeroway"
  | "road"
  | "casing"
  | "path"
  | "rail"
  | "gap"
  | "boundary"
  | "label"
  | "roadLabel"
  | "waterLabel";

const GREEN_CLASSES = /park|wood|forest|grass|scrub|wetland|meadow|garden|cemetery|pitch|golf|farmland/;

/** What a layer of an OpenMapTiles-schema style draws, from its type, source layer and id. */
export function layerRole(layer: maplibregl.LayerSpecification): Role | null {
  const l = layer as unknown as StyleLayer;
  const src = l["source-layer"];
  const id = l.id.toLowerCase();
  if (l.type === "background") return "background";
  if (l.type === "symbol") {
    if (src === "water_name" || src === "waterway") return "waterLabel";
    if (src === "transportation_name") return "roadLabel";
    if (src === "transportation") return null; // one-way arrows
    return "label";
  }
  switch (src) {
    case "water":
      return l.type === "fill" ? "water" : null;
    case "waterway":
      return "waterway";
    case "landcover":
      if (/ice|glacier/.test(id)) return "ice";
      return GREEN_CLASSES.test(id) ? "green" : "landuse";
    case "landuse":
      return GREEN_CLASSES.test(id) ? "green" : "landuse";
    case "park":
      return "green";
    case "building":
      return "building";
    case "aeroway":
      return "aeroway";
    case "boundary":
      return "boundary";
    case "transportation":
      // Piers and the gaps of dashed rail lines are drawn in the ground colour.
      if (l.type === "fill" || /dash|pier/.test(id)) return "gap";
      if (id.includes("rail")) return "rail";
      if (id.includes("path")) return "path";
      if (id.includes("casing")) return "casing";
      return "road";
    default:
      return null;
  }
}

function isZoom(v: unknown): boolean {
  return Array.isArray(v) && v[0] === "zoom";
}

/**
 * Apply `fn` to every output of a zoom curve (or to the value itself). Zoom expressions must
 * stay at the top level, so a data expression can only go inside their outputs.
 */
export function mapOutputs(v: unknown, fn: (out: unknown) => unknown): unknown {
  if (Array.isArray(v) && /^interpolate/.test(String(v[0])) && isZoom(v[2])) {
    return v.map((x, i) => (i >= 4 && i % 2 === 0 ? fn(x) : x));
  }
  if (Array.isArray(v) && v[0] === "step" && isZoom(v[1])) {
    return v.map((x, i) => (i >= 2 && i % 2 === 0 ? fn(x) : x));
  }
  if (v && typeof v === "object" && !Array.isArray(v) && Array.isArray((v as { stops?: unknown }).stops)) {
    const o = v as { stops: [unknown, unknown][] };
    return { ...o, stops: o.stops.map(([z, x]) => [z, fn(x)]) };
  }
  return fn(v);
}

/** Road colour by OpenMapTiles class; tiers the palette leaves out keep the style's colour. */
function roadColorExpr(layerId: string, p: Partial<BasemapPalette>, original: unknown): unknown {
  if (layerId.includes("motorway")) return p.motorway ?? original;
  const tiers: [string[], string | undefined][] = [
    [["motorway"], p.motorway],
    [["trunk", "primary"], p.primary],
    [["secondary", "tertiary"], p.secondary],
  ];
  const defined = tiers.filter(([, c]) => c !== undefined);
  if (defined.length === 0 && p.minor === undefined) return original;
  return mapOutputs(original, (orig) => {
    const expr: unknown[] = ["match", ["get", "class"]];
    for (const [classes, color] of defined) expr.push(classes.length === 1 ? classes[0] : classes, color);
    expr.push(p.minor ?? orig);
    return expr;
  });
}

/** Repaint every layer of an OpenMapTiles-schema style in place, by role (see BasemapPalette). */
export function recolorLayers(layers: maplibregl.LayerSpecification[], p: Partial<BasemapPalette>): void {
  for (const layer of layers as unknown as StyleLayer[]) {
    const role = layerRole(layer as unknown as maplibregl.LayerSpecification);
    if (!role) continue;
    const paint = paintOf(layer);
    const set = (prop: string, color: string | undefined) => {
      if (color !== undefined) paint[prop] = color;
    };
    switch (role) {
      case "background":
        set("background-color", p.background);
        break;
      case "landuse":
      case "green":
      case "ice":
      case "water":
      case "aeroway":
      case "waterway":
      case "boundary":
      case "path":
      case "rail":
      case "casing":
      case "gap": {
        const color = role === "waterway" ? (p.waterway ?? p.water) : role === "gap" ? p.background : p[role];
        if (color === undefined) break;
        if (layer.type === "fill") {
          paint["fill-color"] = color;
          // A texture (the dark style's wood pattern) would hide the palette colour.
          delete paint["fill-pattern"];
          if (paint["fill-outline-color"] !== undefined) paint["fill-outline-color"] = color;
        } else if (layer.type === "line") {
          paint["line-color"] = color;
        }
        break;
      }
      case "building":
        if (layer.type === "fill-extrusion") set("fill-extrusion-color", p.building);
        else {
          set("fill-color", p.building);
          set("fill-outline-color", p.buildingOutline ?? p.building);
        }
        break;
      case "road":
        if (layer.type === "line") paint["line-color"] = roadColorExpr(layer.id.toLowerCase(), p, paint["line-color"]);
        break;
      case "label":
      case "roadLabel":
      case "waterLabel":
        set("text-color", p[role] ?? p.label);
        if (p.labelHalo !== undefined) {
          paint["text-halo-color"] = p.labelHalo;
          if (paint["text-halo-width"] === undefined) paint["text-halo-width"] = 1;
        }
        break;
    }
  }
}

/** Drop the camera a style may carry, so a style swap never moves the map. */
export function stripCamera(style: maplibregl.StyleSpecification): void {
  delete style.center;
  delete style.zoom;
  delete style.bearing;
  delete style.pitch;
}

/** What every style from a custom URL gets: the mute pass, the label fix, no camera. */
export function adaptCustomStyle(style: maplibregl.StyleSpecification): maplibregl.StyleSpecification {
  const out = structuredClone(style);
  stripCamera(out);
  tameLayers(out.layers);
  latinLabelsAtWideZoom(out.layers);
  return out;
}
