/**
 * Push the basemap into the background: every colour is desaturated toward a cool neutral
 * and its contrast against the dark ground reduced, and labels are dimmed. Camera data is
 * then the only saturated thing on screen, while roads and place names stay readable
 * enough to orient by. Applied to whatever style is loaded, before our own layers exist.
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

type PaintMap = {
  getPaintProperty(layer: string, name: string): unknown;
  setPaintProperty(layer: string, name: string, value: unknown): unknown;
  getLayoutProperty(layer: string, name: string): unknown;
  setLayoutProperty(layer: string, name: string, value: unknown): unknown;
};

function setIfPlain(map: PaintMap, id: string, prop: string, value: number): void {
  const cur = map.getPaintProperty(id, prop);
  // Zoom-dependent expressions must stay top-level, so only plain numbers are replaced.
  if (cur === undefined || typeof cur === "number") {
    map.setPaintProperty(id, prop, typeof cur === "number" ? Math.min(cur, value) : value);
  }
}

export function tameBasemap(mapIn: maplibregl.Map): void {
  const map = mapIn as unknown as PaintMap;
  const layers = mapIn.getStyle()?.layers ?? [];
  for (const layer of layers) {
    try {
      for (const prop of COLOR_PROPS[layer.type] ?? []) {
        const v = map.getPaintProperty(layer.id, prop);
        if (v !== undefined) map.setPaintProperty(layer.id, prop, muteValue(v));
      }
      if (layer.type === "symbol") {
        setIfPlain(map, layer.id, "text-opacity", TEXT_OPACITY);
        setIfPlain(map, layer.id, "icon-opacity", ICON_OPACITY);
        const field = map.getLayoutProperty(layer.id, "text-field");
        if (mentions(field, "name:nonlatin") && !mentions(field, "zoom")) {
          map.setLayoutProperty(layer.id, "text-field", ["step", ["zoom"], LATIN_NAME, LATIN_ONLY_BELOW_ZOOM, field]);
        }
      } else if (layer.type === "line") {
        setIfPlain(map, layer.id, "line-opacity", LINE_OPACITY);
      } else if (layer.type === "raster") {
        map.setPaintProperty(layer.id, "raster-saturation", -0.85);
        map.setPaintProperty(layer.id, "raster-contrast", -0.25);
      }
    } catch (e) {
      console.warn(`basemap: could not adjust layer ${layer.id}`, e);
    }
  }
}
