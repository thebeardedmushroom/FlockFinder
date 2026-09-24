/**
 * The camera snapshot the map aggregates: every camera's position, category and filterable
 * fields as typed arrays. Mirrors `src-tauri/src/points.rs`, which documents the layout.
 */
import type { Category } from "./types";

export const MAGIC = "FFP1";
const HEADER_LEN = 16;

export const FLAG_KIND_MASK = 0b11;
export const KIND_FLOCK = 0;
export const KIND_ALPR = 1;
export const FLAG_STALE = 1 << 2;
export const FLAG_TYPE_SHIFT = 4;
const OSM_TYPES = ["node", "way", "relation"] as const;

export interface CameraPoints {
  count: number;
  lon: Float64Array;
  lat: Float64Array;
  osmId: Float64Array;
  /** 0 = no operator; i ≥ 1 → operators[i - 1]. */
  op: Uint32Array;
  flags: Uint8Array;
  operators: string[];
  /** Raw `direction` / `camera:direction` tag by camera index. */
  directions: Map<number, string>;
}

export const EMPTY_POINTS: CameraPoints = {
  count: 0,
  lon: new Float64Array(0),
  lat: new Float64Array(0),
  osmId: new Float64Array(0),
  op: new Uint32Array(0),
  flags: new Uint8Array(0),
  operators: [],
  directions: new Map(),
};

export function decodePoints(buf: ArrayBuffer): CameraPoints {
  const view = new DataView(buf);
  const magic = String.fromCharCode(...new Uint8Array(buf, 0, 4));
  if (magic !== MAGIC) throw new Error(`camera snapshot has an unknown format (${magic})`);
  const n = view.getUint32(4, true);
  const metaLen = view.getUint32(8, true);
  const body = HEADER_LEN + n * 29;
  const metaOff = body + ((4 - (body % 4)) % 4);
  const meta = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, metaOff, metaLen))) as {
    operators: string[];
    directions: [number, string][];
  };
  return {
    count: n,
    lon: new Float64Array(buf, HEADER_LEN, n),
    lat: new Float64Array(buf, HEADER_LEN + 8 * n, n),
    osmId: new Float64Array(buf, HEADER_LEN + 16 * n, n),
    op: new Uint32Array(buf, HEADER_LEN + 24 * n, n),
    flags: new Uint8Array(buf, HEADER_LEN + 28 * n, n),
    operators: meta.operators,
    directions: new Map(meta.directions),
  };
}

export interface EncodableCamera {
  osm_type: string;
  osm_id: number;
  lat: number;
  lon: number;
  category: Category;
  stale: boolean;
  operator: string | null;
  direction: string | null;
}

/** The same layout as the Rust encoder (used by the dev mock and tests). */
export function encodePoints(cams: EncodableCamera[]): ArrayBuffer {
  const n = cams.length;
  const operators: string[] = [];
  const opIndex = new Map<string, number>();
  const directions: [number, string][] = [];
  const ops = new Uint32Array(n);
  const flags = new Uint8Array(n);
  cams.forEach((c, i) => {
    const kind = c.category === "flock" ? KIND_FLOCK : KIND_ALPR;
    const type = Math.max(0, OSM_TYPES.indexOf(c.osm_type as (typeof OSM_TYPES)[number]));
    flags[i] = kind | (c.stale ? FLAG_STALE : 0) | (type << FLAG_TYPE_SHIFT);
    const op = c.operator?.trim();
    if (op) {
      let idx = opIndex.get(op);
      if (idx === undefined) {
        operators.push(op);
        idx = operators.length;
        opIndex.set(op, idx);
      }
      ops[i] = idx;
    }
    if (c.direction && c.direction.trim()) directions.push([i, c.direction]);
  });
  const meta = new TextEncoder().encode(JSON.stringify({ operators, directions }));
  const body = HEADER_LEN + n * 29;
  const metaOff = body + ((4 - (body % 4)) % 4);
  const buf = new ArrayBuffer(metaOff + meta.length);
  const view = new DataView(buf);
  for (let i = 0; i < 4; i++) view.setUint8(i, MAGIC.charCodeAt(i));
  view.setUint32(4, n, true);
  view.setUint32(8, meta.length, true);
  const lon = new Float64Array(buf, HEADER_LEN, n);
  const lat = new Float64Array(buf, HEADER_LEN + 8 * n, n);
  const ids = new Float64Array(buf, HEADER_LEN + 16 * n, n);
  cams.forEach((c, i) => {
    lon[i] = c.lon;
    lat[i] = c.lat;
    ids[i] = c.osm_id;
  });
  new Uint32Array(buf, HEADER_LEN + 24 * n, n).set(ops);
  new Uint8Array(buf, HEADER_LEN + 28 * n, n).set(flags);
  new Uint8Array(buf, metaOff).set(meta);
  return buf;
}

export const pointKind = (flags: number): "flock" | "alpr" =>
  (flags & FLAG_KIND_MASK) === KIND_FLOCK ? "flock" : "alpr";

export const pointStale = (flags: number): boolean => (flags & FLAG_STALE) !== 0;

export const pointOsmType = (flags: number): string => OSM_TYPES[(flags >> FLAG_TYPE_SHIFT) & 0b11] ?? "node";

/** `node/123`, matching `cameraKey`. */
export function pointKey(p: CameraPoints, i: number): string {
  return `${pointOsmType(p.flags[i])}/${p.osmId[i]}`;
}

export function pointOperator(p: CameraPoints, i: number): string | null {
  const o = p.op[i];
  return o === 0 ? null : p.operators[o - 1];
}
