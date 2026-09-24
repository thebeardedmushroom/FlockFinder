/**
 * Aggregation for the camera layer. Runs inside a Web Worker (see
 * `src/workers/cluster.worker.ts`) but is plain TypeScript so tests can drive it directly.
 *
 * `build(filter)` indexes the filtered set once: a Supercluster hierarchy for the cluster
 * bands and hex bins for the wide band. Panning and zooming then only query that index, so
 * nothing is recomputed per viewport change. Because the index is built from the filtered
 * set, every count it reports matches what the filters show.
 *
 * The split/merge animation needs to know which cluster a node came from. Supercluster
 * records that in its trees (`trees[L].data[k + OFFSET_PARENT]` is the id, at level L − 1,
 * of the cluster that row merged into; −1 means it carried over unchanged). This module
 * reads those internals; supercluster is pinned to 8.0.1 and the tests cover the contract.
 */
import Supercluster from "supercluster";
import { FLAG_KIND_MASK, FLAG_STALE, KIND_FLOCK } from "./cameraData";
import { hexCell, hexGrid, hexKey, latToMerc, lonToMerc } from "./hexbin";
import { CLUSTER_MIN_LEVEL, CLUSTER_RADIUS_PX, LEAF_LEVEL, NEAR_MAX_LEVEL, WIDE_MAX_LEVEL } from "./lod";

export type BBoxTuple = [west: number, south: number, east: number, north: number];

export interface EnginePoints {
  lon: Float64Array;
  lat: Float64Array;
  flags: Uint8Array;
  op: Uint32Array;
  operators: string[];
}

/** An unverified submission (the user's own, not in OSM data yet). */
export interface EngineUser {
  lon: number;
  lat: number;
  operator: string | null;
}

export interface EngineFilter {
  flock: boolean;
  alpr: boolean;
  user: boolean;
  operator: string;
}

export interface HexLevel {
  level: number;
  q: Int32Array;
  r: Int32Array;
  count: Uint32Array;
  users: Uint32Array;
  max: number;
}

/** First half of a build: the filtered set and the wide band's hex bins (fast). */
export interface HexResult {
  /** Cameras + unverified submissions included by the filter. */
  included: number;
  usersIncluded: number;
  hexMs: number;
  hex: HexLevel[];
}

/** Second half: the cluster hierarchy for the other bands. */
export interface IndexResult {
  buildMs: number;
  /** Largest cluster count per level, CLUSTER_MIN_LEVEL..NEAR_MAX_LEVEL (index = level). */
  maxCount: number[];
}

export type BuildResult = HexResult & IndexResult;

export interface QueryResult {
  level: number;
  /** Node ids at `level`: cluster ids, or feature indices for single cameras. */
  ids: Float64Array;
  /** Mercator position (f64; exact for single cameras). */
  x: Float64Array;
  y: Float64Array;
  count: Uint32Array;
  users: Uint32Array;
  /** Combined index (cameras first, then submissions) for single nodes, −1 for clusters. */
  leaf: Int32Array;
  /**
   * Zooming in: for each new node, the id of its ancestor at `prev.level`.
   * Zooming out: for each id in `prev.ids`, the id of its ancestor at `level`.
   */
  anc: Float64Array | null;
  dir: "in" | "out" | null;
}

export interface ViewCounts {
  /** Counted nodes (filtered) inside the bbox. */
  visible: number;
  users: number;
  byKind: { flock: number; alpr: number; user: number };
  /** Without filters (stale excluded), for "filters hide N". */
  totalByKind: { flock: number; alpr: number; user: number };
}

interface Props {
  u: number;
}

interface KDTree {
  data: number[];
  range(minX: number, minY: number, maxX: number, maxY: number): number[];
}

type Internals = { trees: KDTree[]; stride: number; clusterProps: Props[] };

const OFFSET_ID = 3;
const OFFSET_PARENT = 4;
const OFFSET_NUM = 5;
const OFFSET_PROP = 6;

const CAMERA_PROPS: Props = { u: 0 };
const USER_PROPS: Props = { u: 1 };

function operatorMatcher(needle: string): ((op: string | null) => boolean) | null {
  const n = needle.trim().toLowerCase();
  if (n === "") return null;
  return (op) => (op ?? "").toLowerCase().includes(n);
}

const wrapLon = (lon: number) => ((((lon + 180) % 360) + 360) % 360) - 180;

/** Mercator query rectangles for a lon/lat bbox, split at the antimeridian. */
export function mercRanges(b: BBoxTuple): [number, number, number, number][] {
  const minY = latToMerc(Math.min(90, b[3]));
  const maxY = latToMerc(Math.max(-90, b[1]));
  if (b[2] - b[0] >= 360) return [[0, minY, 1, maxY]];
  const w = wrapLon(b[0]);
  const e = b[2] === 180 ? 180 : wrapLon(b[2]);
  if (w > e) return [[lonToMerc(w), minY, 1, maxY], [0, minY, lonToMerc(e), maxY]];
  return [[lonToMerc(w), minY, lonToMerc(e), maxY]];
}

function bboxContains(b: BBoxTuple, lon: number, lat: number): boolean {
  if (lat < b[1] || lat > b[3]) return false;
  if (b[2] - b[0] >= 360) return true;
  const w = wrapLon(b[0]);
  const e = b[2] === 180 ? 180 : wrapLon(b[2]);
  const l = wrapLon(lon);
  return w <= e ? l >= w && l <= e : l >= w || l <= e;
}

export class ClusterEngine {
  private pts: EnginePoints | null = null;
  private users: EngineUser[] = [];
  private nCams = 0;
  private index: (Supercluster<Props, Props> & Internals) | null = null;
  /** Combined indices the current filter includes, in feature order. */
  private combined: number[] = [];
  /** Feature index → combined index. */
  private featToCombined = new Int32Array(0);
  private parents: (Map<number, number> | Int32Array | undefined)[] = [];
  private filter: EngineFilter | null = null;

  setData(points: EnginePoints, users: EngineUser[]): void {
    this.pts = points;
    this.users = users;
    this.nCams = points.lon.length;
    this.index = null;
  }

  private lon(c: number): number {
    return c < this.nCams ? this.pts!.lon[c] : this.users[c - this.nCams].lon;
  }

  private lat(c: number): number {
    return c < this.nCams ? this.pts!.lat[c] : this.users[c - this.nCams].lat;
  }

  /** Whether combined index c passes the filter (stale cameras never count). */
  private includer(filter: EngineFilter): (c: number) => boolean {
    const pts = this.pts!;
    const match = operatorMatcher(filter.operator);
    const opMatch = match ? pts.operators.map((o) => match(o)) : null;
    return (c) => {
      if (c >= this.nCams) {
        const u = this.users[c - this.nCams];
        return filter.user && (!match || match(u.operator));
      }
      const f = pts.flags[c];
      if (f & FLAG_STALE) return false;
      const flock = (f & FLAG_KIND_MASK) === KIND_FLOCK;
      if (flock ? !filter.flock : !filter.alpr) return false;
      if (!opMatch) return true;
      const o = pts.op[c];
      return o !== 0 && opMatch[o - 1];
    };
  }

  /** Both halves of a build (tests and callers that don't need the early hex result). */
  build(filter: EngineFilter): BuildResult {
    const hex = this.buildHex(filter);
    return { ...hex, ...this.buildIndex() };
  }

  /** Apply the filter and bin the wide band's hex cells. Queries need `buildIndex` next. */
  buildHex(filter: EngineFilter): HexResult {
    if (!this.pts) throw new Error("no data");
    this.filter = filter;
    this.index = null;
    this.parents = [];
    const t0 = performance.now();
    const include = this.includer(filter);
    const total = this.nCams + this.users.length;
    const combined: number[] = [];
    let usersIncluded = 0;
    for (let c = 0; c < total; c++) {
      if (!include(c)) continue;
      if (c >= this.nCams) usersIncluded++;
      combined.push(c);
    }
    this.combined = combined;
    const hex = this.binHexes(combined);
    return { included: combined.length, usersIncluded, hexMs: performance.now() - t0, hex };
  }

  /** Build the cluster hierarchy for the set chosen by the last `buildHex`. */
  buildIndex(): IndexResult {
    const t0 = performance.now();
    const combined = this.combined;
    const features: Supercluster.PointFeature<Props>[] = combined.map((c) => ({
      type: "Feature",
      geometry: { type: "Point", coordinates: [this.lon(c), this.lat(c)] },
      properties: c >= this.nCams ? USER_PROPS : CAMERA_PROPS,
    }));
    this.featToCombined = Int32Array.from(combined);
    const index = new Supercluster<Props, Props>({
      minZoom: CLUSTER_MIN_LEVEL,
      maxZoom: NEAR_MAX_LEVEL,
      radius: CLUSTER_RADIUS_PX,
      extent: 512,
      minPoints: 2,
      map: (p) => ({ u: p.u }),
      reduce: (acc, p) => {
        acc.u += p.u;
      },
    });
    index.load(features);
    this.index = index as Supercluster<Props, Props> & Internals;
    this.parents = [];
    const buildMs = performance.now() - t0;

    const maxCount: number[] = [];
    for (let L = CLUSTER_MIN_LEVEL; L <= NEAR_MAX_LEVEL; L++) {
      const data = this.index.trees[L].data;
      const stride = this.index.stride;
      let m = 1;
      for (let k = 0; k < data.length; k += stride) if (data[k + OFFSET_NUM] > m) m = data[k + OFFSET_NUM];
      maxCount[L] = m;
    }
    return { buildMs, maxCount };
  }

  private binHexes(combined: number[]): HexLevel[] {
    const n = combined.length;
    const xs = new Float64Array(n);
    const ys = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      xs[i] = lonToMerc(this.lon(combined[i]));
      ys[i] = latToMerc(this.lat(combined[i]));
    }
    const out: HexLevel[] = [];
    for (let level = 0; level <= WIDE_MAX_LEVEL; level++) {
      const g = hexGrid(level);
      const slot = new Map<number, number>();
      const q: number[] = [];
      const r: number[] = [];
      const count: number[] = [];
      const users: number[] = [];
      for (let i = 0; i < n; i++) {
        const [cq, cr] = hexCell(g, xs[i], ys[i]);
        const key = hexKey(g, cq, cr);
        let s = slot.get(key);
        if (s === undefined) {
          s = q.length;
          slot.set(key, s);
          q.push(cq);
          r.push(cr);
          count.push(0);
          users.push(0);
        }
        count[s]++;
        if (combined[i] >= this.nCams) users[s]++;
      }
      let max = 0;
      for (const c of count) if (c > max) max = c;
      out.push({
        level,
        q: Int32Array.from(q),
        r: Int32Array.from(r),
        count: Uint32Array.from(count),
        users: Uint32Array.from(users),
        max,
      });
    }
    return out;
  }

  /** Parent id at level L − 1 of node `id` at level L (itself if it carried over). */
  private parentOf(L: number, id: number): number {
    let m = this.parents[L];
    if (!m) {
      const idx = this.index!;
      const data = idx.trees[L].data;
      const stride = idx.stride;
      if (L === LEAF_LEVEL) {
        const arr = new Int32Array(this.featToCombined.length).fill(-1);
        for (let k = 0; k < data.length; k += stride) arr[data[k + OFFSET_ID]] = data[k + OFFSET_PARENT];
        m = arr;
      } else {
        const map = new Map<number, number>();
        for (let k = 0; k < data.length; k += stride) map.set(data[k + OFFSET_ID], data[k + OFFSET_PARENT]);
        m = map;
      }
      this.parents[L] = m;
    }
    const p = m instanceof Int32Array ? m[id] : m.get(id);
    return p === undefined || p === -1 ? id : p;
  }

  /** Id at level `to` of the node that `id` (at level `from` ≥ `to`) belongs to. */
  ancestor(id: number, from: number, to: number): number {
    let L = Math.min(from, LEAF_LEVEL);
    const stop = Math.max(to, CLUSTER_MIN_LEVEL);
    while (L > stop) {
      id = this.parentOf(L, id);
      L--;
    }
    return id;
  }

  query(bbox: BBoxTuple, level: number, prev: { level: number; ids: Float64Array } | null): QueryResult {
    const empty: QueryResult = {
      level,
      ids: new Float64Array(0),
      x: new Float64Array(0),
      y: new Float64Array(0),
      count: new Uint32Array(0),
      users: new Uint32Array(0),
      leaf: new Int32Array(0),
      anc: null,
      dir: null,
    };
    const idx = this.index;
    if (!idx || level < CLUSTER_MIN_LEVEL) return empty;
    const L = Math.min(level, LEAF_LEVEL);
    const data = idx.trees[L].data;
    const stride = idx.stride;
    const ids: number[] = [];
    const xs: number[] = [];
    const ys: number[] = [];
    const counts: number[] = [];
    const users: number[] = [];
    const leaves: number[] = [];
    for (const [minX, minY, maxX, maxY] of mercRanges(bbox)) {
      for (const row of idx.trees[L].range(minX, minY, maxX, maxY)) {
        const k = row * stride;
        const num = data[k + OFFSET_NUM];
        const id = data[k + OFFSET_ID];
        ids.push(id);
        counts.push(num);
        if (num > 1) {
          xs.push(data[k]);
          ys.push(data[k + 1]);
          users.push(idx.clusterProps[data[k + OFFSET_PROP]]?.u ?? 0);
          leaves.push(-1);
        } else {
          const c = this.featToCombined[id];
          xs.push(lonToMerc(this.lon(c)));
          ys.push(latToMerc(this.lat(c)));
          users.push(c >= this.nCams ? 1 : 0);
          leaves.push(c);
        }
      }
    }
    const res: QueryResult = {
      level,
      ids: Float64Array.from(ids),
      x: Float64Array.from(xs),
      y: Float64Array.from(ys),
      count: Uint32Array.from(counts),
      users: Uint32Array.from(users),
      leaf: Int32Array.from(leaves),
      anc: null,
      dir: null,
    };
    if (prev && prev.level >= CLUSTER_MIN_LEVEL && Math.min(prev.level, LEAF_LEVEL) !== L) {
      if (prev.level < level) {
        res.dir = "in";
        res.anc = Float64Array.from(ids, (id) => this.ancestor(id, L, prev.level));
      } else {
        res.dir = "out";
        res.anc = Float64Array.from(prev.ids, (id) => this.ancestor(id, prev.level, L));
      }
    }
    return res;
  }

  counts(bbox: BBoxTuple): ViewCounts {
    const out: ViewCounts = {
      visible: 0,
      users: 0,
      byKind: { flock: 0, alpr: 0, user: 0 },
      totalByKind: { flock: 0, alpr: 0, user: 0 },
    };
    const pts = this.pts;
    if (!pts || !this.filter) return out;
    const include = this.includer(this.filter);
    for (let c = 0; c < this.nCams; c++) {
      const f = pts.flags[c];
      if (f & FLAG_STALE) continue;
      if (!bboxContains(bbox, pts.lon[c], pts.lat[c])) continue;
      const kind = (f & FLAG_KIND_MASK) === KIND_FLOCK ? "flock" : "alpr";
      out.totalByKind[kind]++;
      if (include(c)) {
        out.byKind[kind]++;
        out.visible++;
      }
    }
    for (let j = 0; j < this.users.length; j++) {
      const u = this.users[j];
      if (!bboxContains(bbox, u.lon, u.lat)) continue;
      out.totalByKind.user++;
      if (include(this.nCams + j)) {
        out.byKind.user++;
        out.users++;
        out.visible++;
      }
    }
    return out;
  }

  /** Bounds of a cluster's cameras and the zoom at which it splits. */
  expand(clusterId: number): { bounds: BBoxTuple; zoom: number } | null {
    const idx = this.index;
    if (!idx) return null;
    try {
      const leaves = idx.getLeaves(clusterId, Infinity);
      let w = 180;
      let s = 90;
      let e = -180;
      let n = -90;
      for (const f of leaves) {
        const [lon, lat] = f.geometry.coordinates;
        w = Math.min(w, lon);
        e = Math.max(e, lon);
        s = Math.min(s, lat);
        n = Math.max(n, lat);
      }
      return { bounds: [w, s, e, n], zoom: idx.getClusterExpansionZoom(clusterId) };
    } catch {
      return null; // stale id from a previous build
    }
  }
}
