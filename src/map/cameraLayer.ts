/**
 * The camera layer: every camera at every zoom level, drawn by level of detail (see
 * `lib/lod.ts` for the bands). A MapLibre custom layer renders cluster nodes, individual
 * cameras and direction cones with instanced WebGL; a 2D canvas on top draws count labels
 * (collision-checked every frame), callout hairlines and the load sweep. The wide band's
 * hex density field is two ordinary MapLibre fill layers, crossfaded between levels.
 *
 * Aggregation lives in a worker (`clusterClient.ts`): one index per data/filter change,
 * queried as the map moves. Only one query is in flight; newer needs coalesce, and an answer
 * for a level the map has already left is discarded rather than animated.
 */
import type * as GeoJSON from "geojson";
import * as maplibregl from "maplibre-gl";
import { EMPTY_POINTS, pointKind, pointOsmType, pointStale, type CameraPoints } from "../lib/cameraData";
import type { BBoxTuple, EngineFilter, HexResult, IndexResult, QueryResult, ViewCounts } from "../lib/clusterEngine";
import { DEFAULT_FOV, MAX_CONES, parseDirectionValue, type Cone } from "../lib/direction";
import { hexCell, hexCenter, hexGrid, hexKey, hexWidthKm, latToMerc, lonToMerc, mercToLat, mercToLon } from "../lib/hexbin";
import { anyOverlap, placeLabels, type LabelRequest } from "../lib/labels";
import {
  bandForLevel,
  CLUSTER_MIN_LEVEL,
  CONE_MIN_LEVEL,
  CONE_RADIUS_PX,
  coneScale,
  COUNT_THROTTLE_MS,
  densityT,
  FADE_IN_MS,
  FILTER_DEBOUNCE_MS,
  formatCount,
  HALO_MAX_INTENSITY,
  HALO_SCALE,
  LEAF_LEVEL,
  levelForZoom,
  levelScale,
  minProportionalCount,
  NEAR_MAX_LEVEL,
  nodeRadius,
  POINT_RADIUS_PX,
  PULSE_CYCLES,
  PULSE_MAX_NODES,
  PULSE_MIN_T,
  PULSE_PERIOD_MS,
  QUERY_PAD,
  SPIDER_MIN_LEVEL,
  SPIDER_PICK_PX,
  SWEEP_MS,
  TRANSITION_MS,
  TRANSITION_REDUCED_MS,
  WIDE_MAX_LEVEL,
  type Band,
} from "../lib/lod";
import { relativeLuminance, type Rgb } from "../lib/ramp";
import { ClusterClient } from "./clusterClient";
import { DARK_OVERLAY, type OverlayPalette } from "./themes";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export interface LodState {
  band: Band;
  level: number;
  /** px per √camera at this level (cluster bands). */
  scale: number;
  /** Largest cluster at this level, whole filtered dataset. */
  maxCount: number;
  /** Counts below this are drawn at the minimum size. */
  minProportional: number;
  /** Densest hex at this level (wide band). */
  hexMax: number;
  hexWidthKm: number;
  building: boolean;
  /** Cameras + unverified submissions the filters include, worldwide. */
  included: number;
  usersIncluded: number;
}

export interface SubmissionPoint {
  id: number;
  lat: number;
  lon: number;
  operator: string | null;
  direction: number | null;
  status: "local" | "uploaded";
  osm_element_id: number | null;
}

export interface CameraLayerEvents {
  onLod(state: LodState): void;
  onCounts(counts: ViewCounts): void;
  /** A camera (combined index < points.count) or submission (≥) was clicked. */
  onPickCamera(index: number): void;
  onPickSubmission(id: number): void;
  onBuilt(info: { included: number; buildMs: number; hexMs: number; first: boolean }): void;
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

interface DNode {
  id: number;
  leaf: number;
  count: number;
  users: number;
  t: number;
  // animation: from → to, over [start, start + dur]
  fx: number;
  fy: number;
  fr: number;
  fa: number;
  tx: number;
  ty: number;
  tr: number;
  ta: number;
  start: number;
  dur: number;
  dying: boolean;
}

interface FrameNode {
  n: DNode;
  sx: number;
  sy: number;
  r: number;
  a: number;
}

const easeOutCubic = (p: number) => 1 - (1 - p) ** 3;

function current(n: DNode, now: number): { x: number; y: number; r: number; a: number; p: number } {
  const p = n.dur > 0 ? Math.min(1, Math.max(0, (now - n.start) / n.dur)) : 1;
  const e = easeOutCubic(p);
  return {
    x: n.fx + (n.tx - n.fx) * e,
    y: n.fy + (n.ty - n.fy) * e,
    r: n.fr + (n.tr - n.fr) * e,
    a: n.fa + (n.ta - n.fa) * e,
    p,
  };
}

// ---- shaders ----

const VS_HALO = `#version 300 es
layout(location=0) in vec2 a_corner;
layout(location=1) in vec2 i_pos;
layout(location=2) in float i_size;
layout(location=3) in vec3 i_color;
layout(location=4) in float i_intensity;
uniform vec2 u_viewport;
out vec2 v_uv; out vec3 v_color; out float v_intensity;
void main() {
  vec2 px = i_pos + a_corner * i_size;
  vec2 ndc = px / u_viewport * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
  v_uv = a_corner; v_color = i_color; v_intensity = i_intensity;
}`;

const FS_HALO = `#version 300 es
precision mediump float;
in vec2 v_uv; in vec3 v_color; in float v_intensity;
out vec4 fragColor;
void main() {
  float d2 = dot(v_uv, v_uv);
  if (d2 >= 1.0) discard;
  // Bright core, soft Gaussian falloff reaching zero at the quad edge.
  float g = max(exp(-d2 * 4.5) - 0.0111, 0.0) * v_intensity;
  fragColor = vec4(v_color * g, g);
}`;

const VS_NODE = `#version 300 es
layout(location=0) in vec2 a_corner;
layout(location=1) in vec2 i_pos;
layout(location=2) in float i_radius;
layout(location=3) in vec4 i_fill;
layout(location=4) in vec4 i_stroke;
layout(location=5) in float i_style;
layout(location=6) in float i_ring2;
uniform vec2 u_viewport;
out vec2 v_px; out float v_r; out vec4 v_fill; out vec4 v_stroke; out float v_style; out float v_ring2;
void main() {
  float ext = i_radius + 4.0;
  vec2 px = i_pos + a_corner * ext;
  vec2 ndc = px / u_viewport * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
  v_px = a_corner * ext; v_r = i_radius; v_fill = i_fill; v_stroke = i_stroke; v_style = i_style; v_ring2 = i_ring2;
}`;

const FS_NODE = `#version 300 es
precision highp float;
in vec2 v_px; in float v_r; in vec4 v_fill; in vec4 v_stroke; in float v_style; in float v_ring2;
uniform float u_aa;
uniform vec3 u_state;
out vec4 fragColor;
void main() {
  float d = length(v_px);
  float sw = v_style > 0.5 ? 1.6 : 1.0;
  float outer = 1.0 - smoothstep(v_r - u_aa, v_r + u_aa, d);
  float inner = 1.0 - smoothstep(v_r - sw - u_aa, v_r - sw + u_aa, d);
  vec4 col = v_fill * inner + v_stroke * max(outer - inner, 0.0);
  if (v_ring2 > 0.0) {
    // Hairline ring in the state hue: this cluster includes unverified submissions.
    float ring = (1.0 - smoothstep(0.5, 0.5 + 1.5 * u_aa, abs(d - (v_r + 2.5)))) * v_ring2;
    col += vec4(u_state * ring, ring) * (1.0 - col.a);
  }
  if (col.a < 0.003) discard;
  fragColor = col;
}`;

const VS_CONE = `#version 300 es
layout(location=0) in vec2 a_corner;
layout(location=1) in vec2 i_pos;
layout(location=2) in float i_radius;
layout(location=3) in float i_bearing;
layout(location=4) in float i_half;
layout(location=5) in vec3 i_color;
layout(location=6) in float i_alpha;
uniform vec2 u_viewport;
out vec2 v_uv; out float v_r; out float v_bearing; out float v_half; out vec3 v_color; out float v_alpha;
void main() {
  vec2 px = i_pos + a_corner * i_radius;
  vec2 ndc = px / u_viewport * 2.0 - 1.0;
  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
  v_uv = a_corner; v_r = i_radius; v_bearing = i_bearing; v_half = i_half; v_color = i_color; v_alpha = i_alpha;
}`;

const FS_CONE = `#version 300 es
precision highp float;
in vec2 v_uv; in float v_r; in float v_bearing; in float v_half; in vec3 v_color; in float v_alpha;
uniform float u_aa;
out vec4 fragColor;
void main() {
  float d = length(v_uv);
  if (d > 1.0) discard;
  float ang = atan(v_uv.x, -v_uv.y); // clockwise from screen-up
  float diff = abs(mod(ang - v_bearing + 3.14159265, 6.28318531) - 3.14159265);
  float e = clamp(u_aa / max(d * v_r, 0.5), 0.002, 0.35);
  float inside = v_half >= 3.14 ? 1.0 : 1.0 - smoothstep(v_half - e, v_half + e, diff);
  float rim = 1.0 - smoothstep(1.0 - 2.0 * u_aa / v_r, 1.0, d);
  float fill = mix(0.42, 0.03, d);
  float edge = v_half >= 3.14 ? 0.0 : (1.0 - smoothstep(0.0, 1.5 * e, abs(diff - v_half))) * 0.35 * (1.0 - d);
  float a = (inside * fill + edge) * rim * v_alpha;
  fragColor = vec4(v_color * a, a);
}`;

// Hex density field: one instance per cell, a 6-triangle fan per instance, in mercator.
const VS_HEX = `#version 300 es
layout(location=0) in vec2 a_corner;
layout(location=1) in float a_edge;
layout(location=2) in vec3 i_hex;
layout(location=3) in vec4 i_fill;
layout(location=4) in vec4 i_line;
uniform mat4 u_matrix;
uniform float u_wrap;
uniform float u_alpha;
out float v_edge; out vec4 v_fill; out vec4 v_line;
void main() {
  vec2 p = i_hex.xy + a_corner * i_hex.z;
  gl_Position = u_matrix * vec4(p.x + u_wrap, p.y, 0.0, 1.0);
  v_edge = a_edge; v_fill = i_fill * u_alpha; v_line = i_line * u_alpha;
}`;

const FS_HEX = `#version 300 es
precision highp float;
in float v_edge; in vec4 v_fill; in vec4 v_line;
out vec4 fragColor;
void main() {
  // v_edge is 0 at the centre and 1 along the rim, so its screen-space derivative gives a
  // hairline outline without a separate line pass.
  float w = fwidth(v_edge);
  float rim = smoothstep(1.0 - 1.8 * w, 1.0 - 0.6 * w, v_edge);
  vec4 line = v_line * rim;
  fragColor = line + v_fill * (1.0 - line.a);
}`;

/** Floats per hex instance: centre x, y, circumradius, fill rgba, line rgba (premultiplied). */
const HEX_STRIDE = 11;
const HEX_FILL_OPACITY = 0.92;
const HEX_LINE_OPACITY = 0.5;

interface HexDraw {
  level: number;
  gen: number;
  data: Float32Array;
  n: number;
  glBuf: WebGLBuffer | null;
  from: number;
  to: number;
  start: number;
  dur: number;
  /** Colours changed (theme switch): replace the GPU copy on the next frame. */
  reupload?: boolean;
}

function hexAlpha(d: HexDraw, now: number): { a: number; p: number } {
  const p = d.dur > 0 ? Math.min(1, Math.max(0, (now - d.start) / d.dur)) : 1;
  return { a: d.from + (d.to - d.from) * easeOutCubic(p), p };
}

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const s = gl.createShader(type)!;
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`shader: ${gl.getShaderInfoLog(s)}`);
  return s;
}

function link(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
  const p = gl.createProgram()!;
  gl.attachShader(p, compile(gl, gl.VERTEX_SHADER, vs));
  gl.attachShader(p, compile(gl, gl.FRAGMENT_SHADER, fs));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(`program: ${gl.getProgramInfoLog(p)}`);
  return p;
}

/** One instanced draw: a static unit quad plus an interleaved per-instance buffer. */
class Batch {
  vao: WebGLVertexArrayObject;
  buf: WebGLBuffer;
  stride: number;
  data = new Float32Array(1024);
  n = 0;

  constructor(gl: WebGL2RenderingContext, corner: WebGLBuffer, sizes: number[]) {
    this.stride = sizes.reduce((a, b) => a + b, 0);
    this.vao = gl.createVertexArray()!;
    this.buf = gl.createBuffer()!;
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, corner);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    let off = 0;
    sizes.forEach((size, i) => {
      gl.enableVertexAttribArray(i + 1);
      gl.vertexAttribPointer(i + 1, size, gl.FLOAT, false, this.stride * 4, off * 4);
      gl.vertexAttribDivisor(i + 1, 1);
      off += size;
    });
    gl.bindVertexArray(null);
  }

  reset(): void {
    this.n = 0;
  }

  push(...v: number[]): void {
    const need = (this.n + 1) * this.stride;
    if (need > this.data.length) {
      const next = new Float32Array(Math.max(need, this.data.length * 2));
      next.set(this.data);
      this.data = next;
    }
    this.data.set(v, this.n * this.stride);
    this.n++;
  }

  draw(gl: WebGL2RenderingContext): void {
    if (this.n === 0) return;
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, this.data.subarray(0, this.n * this.stride), gl.DYNAMIC_DRAW);
    gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, this.n);
  }

  destroy(gl: WebGL2RenderingContext): void {
    gl.deleteVertexArray(this.vao);
    gl.deleteBuffer(this.buf);
  }
}

interface GlState {
  corner: WebGLBuffer;
  halo: { prog: WebGLProgram; batch: Batch; uViewport: WebGLUniformLocation | null };
  node: { prog: WebGLProgram; batch: Batch; uViewport: WebGLUniformLocation | null; uAa: WebGLUniformLocation | null; uState: WebGLUniformLocation | null };
  cone: { prog: WebGLProgram; batch: Batch; uViewport: WebGLUniformLocation | null; uAa: WebGLUniformLocation | null };
}

const LABEL_FONT_PX = 11;
const LABEL_H = 15;
const LABEL_PAD = 4;
const MONO = `"Cascadia Mono", Consolas, "SF Mono", "JetBrains Mono", ui-monospace, Menlo, monospace`;

function rgbStr(c: Rgb, a = 1): string {
  return `rgba(${Math.round(c[0] * 255)}, ${Math.round(c[1] * 255)}, ${Math.round(c[2] * 255)}, ${a})`;
}

function containsBBox(outer: BBoxTuple, inner: BBoxTuple): boolean {
  const lonOk = outer[2] - outer[0] >= 360 || (inner[0] >= outer[0] && inner[2] <= outer[2]);
  return lonOk && inner[1] >= outer[1] && inner[3] <= outer[3];
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

export class CameraLayer implements maplibregl.CustomLayerInterface {
  readonly id = "cameras";
  readonly type = "custom" as const;
  readonly renderingMode = "2d" as const;

  private map: maplibregl.Map | null = null;
  private gl: GlState | null = null;
  private client = new ClusterClient();
  private events: CameraLayerEvents;

  private points: CameraPoints = EMPTY_POINTS;
  private subs: SubmissionPoint[] = [];
  private filter: EngineFilter = { flock: true, alpr: true, user: true, operator: "" };
  private cones = true;
  private hasData = false;
  /** Colours for the current map theme. */
  private pal: OverlayPalette = DARK_OVERLAY;

  /** Filtered set + hex bins of the latest build (the wide band needs only this). */
  private hexData: HexResult | null = null;
  private hexGen = 0;
  /** Cluster index of the latest build; `gen` is the generation queries answer for. */
  private index: IndexResult | null = null;
  private gen = 0;
  /** Instance data per `${gen}:${level}`, built on first use. */
  private hexInstCache = new Map<string, { data: Float32Array; n: number }>();
  /** Cell key → index per level, for the hover readout. */
  private hexIndex = new Map<number, Map<number, number>>();
  /** The field on screen: the current level last, older ones fading out before it. */
  private hexDraws: HexDraw[] = [];
  private hexGl: {
    prog: WebGLProgram;
    vao: WebGLVertexArrayObject;
    geom: WebGLBuffer;
    uMatrix: WebGLUniformLocation | null;
    uWrap: WebGLUniformLocation | null;
    uAlpha: WebGLUniformLocation | null;
  } | null = null;

  /**
   * The wide band's density field, a second custom layer added under the basemap's labels.
   * It draws from one static buffer per level, so unlike a GeoJSON source it creates no
   * tiles: panning into new areas costs the main thread nothing for the field.
   */
  readonly hexLayer: maplibregl.CustomLayerInterface = {
    id: "camera-hexes",
    type: "custom",
    renderingMode: "2d",
    onAdd: (_map, gl) => this.onHexAdd(gl),
    onRemove: (_map, gl) => this.onHexRemove(gl),
    render: (gl, options) => this.renderHexes(gl, options),
  };

  private nodes: DNode[] = [];
  private nodesGen = -1;
  private displayLevel: number | null = null;
  private inflight = false;
  private pendingQuery = false;
  private lastQuery: { level: number; bbox: BBoxTuple; gen: number } | null = null;

  private frame: FrameNode[] = [];
  private spider: { level: number; x: number; y: number; leaves: number[] } | null = null;
  private pulse: { start: number; ids: Set<number> } | null = null;
  private pulseTimer: number | null = null;
  private sweep: { start: number } | null = null;
  private sweepDone = false;
  private reducedMotion = false;
  private motionQuery: MediaQueryList | null = null;

  private canvas: HTMLCanvasElement | null = null;
  private ctx: CanvasRenderingContext2D | null = null;
  private charW = 6.6;
  private coneCache = new Map<number, Cone[]>();
  private countsTimer: number | null = null;
  private countsDue = 0;
  private filterTimer: number | null = null;
  private lastLodKey = "";
  private listening = false;
  private matrixChecked = false;
  private useMatrix = false;

  /** Dev instrumentation (read through window.__ff). */
  readonly stats = {
    lastBuildMs: 0,
    lastHexMs: 0,
    firstDataFrameAt: 0,
    drawn: 0,
    projection: "",
    /** Level transitions: `anim` = animation start → settled, `total` = level crossed → settled (ms). */
    transitions: [] as { from: number | null; to: number; anim: number; total: number }[],
    /** Count labels in the last frame: requested, placed, and whether any two overlapped. */
    labels: { requested: 0, placed: 0, inside: 0, overlaps: false },
    sweeps: 0,
    pulses: 0,
  };
  /** Dev only: stretch animations to capture intermediate frames (1 = real speed). */
  debugTimeScale = 1;
  private levelSeen: number | null = null;
  private levelCrossAt = 0;
  private transition: { from: number | null; to: number; start: number; crossedAt: number } | null = null;

  constructor(events: CameraLayerEvents) {
    this.events = events;
    this.client.onHex = (gen, r) => this.onHex(gen, r);
    this.client.onBuilt = (gen, r) => this.onIndex(gen, r);
    if (typeof window !== "undefined" && window.matchMedia) {
      this.motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
      this.reducedMotion = this.motionQuery.matches;
      this.motionQuery.addEventListener("change", this.onMotionPref);
    }
  }

  private onMotionPref = (e: MediaQueryListEvent) => {
    this.reducedMotion = e.matches;
    if (e.matches) {
      this.pulse = null;
      this.sweep = null;
    }
  };

  // ---- style integration -------------------------------------------------

  /** Recolour for a new map theme. Data, level and animations carry on untouched. */
  setPalette(pal: OverlayPalette): void {
    if (pal === this.pal) return;
    this.pal = pal;
    this.hexInstCache.clear();
    for (const d of this.hexDraws) {
      if (d.gen !== this.hexGen || !this.hexData) continue;
      const inst = this.hexInstances(d.level);
      Object.assign(d, { data: inst.data, n: inst.n, reupload: true });
    }
    if (this.map?.getLayer("cam-stale")) this.map.setPaintProperty("cam-stale", "circle-stroke-color", pal.stale);
    this.map?.triggerRepaint();
  }

  /** Add the density field and the stale-camera layer; call on every style load, before addLayer(this). */
  installStyleLayers(map: maplibregl.Map, beforeId?: string): void {
    if (map.getLayer(this.hexLayer.id)) return;
    this.map = map;
    map.addLayer(this.hexLayer, beforeId);
    if (!map.getSource("cam-stale")) map.addSource("cam-stale", { type: "geojson", data: { type: "FeatureCollection", features: [] } });
    map.addLayer(
      {
        id: "cam-stale",
        type: "circle",
        source: "cam-stale",
        minzoom: LEAF_LEVEL,
        paint: {
          "circle-radius": POINT_RADIUS_PX,
          "circle-color": "rgba(0,0,0,0)",
          "circle-stroke-color": this.pal.stale,
          "circle-stroke-width": 1,
        },
      },
      beforeId,
    );
    this.refreshStale();
  }

  onAdd(map: maplibregl.Map, gl: WebGL2RenderingContext): void {
    this.map = map;
    const corner = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, corner);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const halo = link(gl, VS_HALO, FS_HALO);
    const node = link(gl, VS_NODE, FS_NODE);
    const cone = link(gl, VS_CONE, FS_CONE);
    this.gl = {
      corner,
      halo: { prog: halo, batch: new Batch(gl, corner, [2, 1, 3, 1]), uViewport: gl.getUniformLocation(halo, "u_viewport") },
      node: {
        prog: node,
        batch: new Batch(gl, corner, [2, 1, 4, 4, 1, 1]),
        uViewport: gl.getUniformLocation(node, "u_viewport"),
        uAa: gl.getUniformLocation(node, "u_aa"),
        uState: gl.getUniformLocation(node, "u_state"),
      },
      cone: {
        prog: cone,
        batch: new Batch(gl, corner, [2, 1, 1, 1, 3, 1]),
        uViewport: gl.getUniformLocation(cone, "u_viewport"),
        uAa: gl.getUniformLocation(cone, "u_aa"),
      },
    };
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    if (!this.canvas) {
      const c = document.createElement("canvas");
      c.className = "camera-labels";
      map.getCanvasContainer().appendChild(c);
      this.canvas = c;
      this.ctx = c.getContext("2d");
      if (this.ctx) {
        this.ctx.font = `600 ${LABEL_FONT_PX}px ${MONO}`;
        this.charW = this.ctx.measureText("0").width || this.charW;
      }
    }
    if (!this.listening) {
      this.listening = true;
      map.on("move", this.onMove);
      map.on("moveend", this.onMoveEnd);
      map.on("movestart", this.onMoveStart);
    }
    this.onMove();
  }

  onRemove(_map: maplibregl.Map, gl: WebGL2RenderingContext): void {
    const g = this.gl;
    if (!g) return;
    g.halo.batch.destroy(gl);
    g.node.batch.destroy(gl);
    g.cone.batch.destroy(gl);
    gl.deleteProgram(g.halo.prog);
    gl.deleteProgram(g.node.prog);
    gl.deleteProgram(g.cone.prog);
    gl.deleteBuffer(g.corner);
    this.gl = null;
  }

  destroy(): void {
    this.client.destroy();
    this.motionQuery?.removeEventListener("change", this.onMotionPref);
    if (this.map && this.listening) {
      this.map.off("move", this.onMove);
      this.map.off("moveend", this.onMoveEnd);
      this.map.off("movestart", this.onMoveStart);
    }
    this.canvas?.remove();
    this.canvas = null;
    if (this.countsTimer) window.clearTimeout(this.countsTimer);
    if (this.filterTimer) window.clearTimeout(this.filterTimer);
    if (this.pulseTimer) window.clearTimeout(this.pulseTimer);
  }

  // ---- data --------------------------------------------------------------

  setData(points: CameraPoints, submissions: SubmissionPoint[]): void {
    this.points = points;
    this.coneCache.clear();
    const nodeIds = new Set<number>();
    for (let i = 0; i < points.count; i++) if (pointOsmType(points.flags[i]) === "node") nodeIds.add(points.osmId[i]);
    // A submission already uploaded and present in the synced data is that camera; don't count it twice.
    this.subs = submissions.filter((s) => !(s.status === "uploaded" && s.osm_element_id !== null && nodeIds.has(s.osm_element_id)));
    this.client.setData(
      { lon: points.lon, lat: points.lat, flags: points.flags, op: points.op, operators: points.operators },
      this.subs.map((s) => ({ lon: s.lon, lat: s.lat, operator: s.operator })),
    );
    this.hasData = true;
    this.rebuild();
  }

  setFilter(filter: EngineFilter): void {
    if (
      filter.flock === this.filter.flock &&
      filter.alpr === this.filter.alpr &&
      filter.user === this.filter.user &&
      filter.operator.trim().toLowerCase() === this.filter.operator.trim().toLowerCase()
    ) {
      return;
    }
    this.filter = { ...filter };
    if (this.filterTimer) window.clearTimeout(this.filterTimer);
    this.filterTimer = window.setTimeout(() => {
      this.filterTimer = null;
      this.rebuild();
    }, FILTER_DEBOUNCE_MS);
  }

  setCones(on: boolean): void {
    this.cones = on;
    this.map?.triggerRepaint();
  }

  private rebuild(): void {
    if (!this.hasData) return;
    this.client.build(this.filter);
    this.emitLod(true);
  }

  private onHex(gen: number, r: HexResult): void {
    const first = this.hexData === null;
    this.hexData = r;
    this.hexGen = gen;
    // Draws already on screen keep their own data and crossfade to the new generation.
    this.hexInstCache.clear();
    this.hexIndex.clear();
    this.stats.lastHexMs = r.hexMs;
    this.refreshStale();
    if (first && !this.sweepDone && !this.reducedMotion && r.included > 0) {
      this.sweep = { start: performance.now() };
      this.stats.sweeps++;
    }
    if (first) this.sweepDone = true;
    this.onMove();
    this.scheduleCounts(true);
    this.map?.triggerRepaint();
  }

  private onIndex(gen: number, r: IndexResult): void {
    const first = this.index === null;
    this.index = r;
    this.gen = gen;
    this.lastQuery = null;
    this.stats.lastBuildMs = r.buildMs;
    this.events.onBuilt({ included: this.hexData?.included ?? 0, buildMs: r.buildMs, hexMs: this.hexData?.hexMs ?? 0, first });
    this.onMove();
    this.emitLod(false);
    this.map?.triggerRepaint();
  }

  private refreshStale(): void {
    const src = this.map?.getSource("cam-stale") as maplibregl.GeoJSONSource | undefined;
    if (!src) return;
    const p = this.points;
    const features: GeoJSON.Feature[] = [];
    for (let i = 0; i < p.count; i++) {
      if (!pointStale(p.flags[i])) continue;
      if (!this.filter[pointKind(p.flags[i])]) continue;
      features.push({ type: "Feature", geometry: { type: "Point", coordinates: [p.lon[i], p.lat[i]] }, properties: { i } });
    }
    src.setData({ type: "FeatureCollection", features });
  }

  // ---- level of detail ---------------------------------------------------

  private scaleFor(level: number): number {
    const L = Math.min(Math.max(level, CLUSTER_MIN_LEVEL), NEAR_MAX_LEVEL);
    return levelScale(this.index?.maxCount[L] ?? 1);
  }

  private maxFor(level: number): number {
    const L = Math.min(Math.max(level, CLUSTER_MIN_LEVEL), NEAR_MAX_LEVEL);
    return this.index?.maxCount[L] ?? 1;
  }

  private emitLod(building: boolean): void {
    const map = this.map;
    if (!map) return;
    const level = levelForZoom(map.getZoom());
    const hexLevel = Math.min(level, WIDE_MAX_LEVEL);
    const scale = this.scaleFor(level);
    const state: LodState = {
      band: bandForLevel(level),
      level,
      scale,
      maxCount: this.maxFor(level),
      minProportional: minProportionalCount(scale),
      hexMax: this.hexData?.hex[hexLevel]?.max ?? 0,
      hexWidthKm: hexWidthKm(hexGrid(hexLevel), map.getCenter().lat),
      building,
      included: this.hexData?.included ?? 0,
      usersIncluded: this.hexData?.usersIncluded ?? 0,
    };
    const key = `${state.band}|${state.level}|${state.building}|${this.gen}|${this.hexGen}|${Math.round(state.hexWidthKm)}`;
    if (key === this.lastLodKey) return;
    this.lastLodKey = key;
    this.events.onLod(state);
  }

  private onMoveStart = () => {
    this.pulse = null;
    if (this.pulseTimer) {
      window.clearTimeout(this.pulseTimer);
      this.pulseTimer = null;
    }
  };

  private onMove = () => {
    const map = this.map;
    if (!map) return;
    const level = levelForZoom(map.getZoom());
    if (level !== this.levelSeen) {
      this.levelSeen = level;
      this.levelCrossAt = performance.now();
    }
    if (this.spider && level !== this.spider.level) this.spider = null;
    if (level <= WIDE_MAX_LEVEL) {
      if (this.displayLevel !== null && this.displayLevel > WIDE_MAX_LEVEL) this.fadeOutAll();
      this.displayLevel = level;
      this.showHex(level);
    } else {
      this.showHex(null);
      this.scheduleQuery();
    }
    this.emitLod(this.client.gen !== this.gen);
    this.scheduleCounts(false);
  };

  private onMoveEnd = () => {
    this.scheduleCounts(true);
    if (this.pulseTimer) window.clearTimeout(this.pulseTimer);
    if (this.reducedMotion) return;
    this.pulseTimer = window.setTimeout(() => {
      this.pulseTimer = null;
      this.startPulse();
    }, TRANSITION_MS + 120);
  };

  private viewBBox(pad: number): BBoxTuple {
    const b = this.map!.getBounds();
    let w = b.getWest();
    let e = b.getEast();
    const dx = (e - w) * pad;
    const s0 = b.getSouth();
    const n0 = b.getNorth();
    const dy = (n0 - s0) * pad;
    w -= dx;
    e += dx;
    const s = Math.max(-85.06, s0 - dy);
    const n = Math.min(85.06, n0 + dy);
    if (e - w >= 360) return [-180, s, 180, n];
    return [w, s, e, n];
  }

  // ---- wide band: hex density field ----------------------------------------

  /** Per-cell instance data for a level: sparse cells fade toward the ground, dense ones are nearly opaque. */
  private hexInstances(level: number): { data: Float32Array; n: number } {
    const key = `${this.hexGen}:${level}`;
    const cached = this.hexInstCache.get(key);
    if (cached) return cached;
    const h = this.hexData!.hex[level];
    const g = hexGrid(level);
    const n = h.q.length;
    const data = new Float32Array(n * HEX_STRIDE);
    for (let i = 0; i < n; i++) {
      const [cx, cy] = hexCenter(g, h.q[i], h.r[i]);
      const t = densityT(h.count[i], h.max);
      const ramp = this.pal.ramp;
      const f = ramp(t);
      const fa = (0.22 + 0.7 * t) * HEX_FILL_OPACITY;
      // Hairline outlines a step brighter than their fill.
      const l = ramp(Math.min(1, t + 0.22));
      const la = HEX_LINE_OPACITY;
      data.set([cx, cy, g.R, f[0] * fa, f[1] * fa, f[2] * fa, fa, l[0] * la, l[1] * la, l[2] * la, la], i * HEX_STRIDE);
    }
    const inst = { data, n };
    this.hexInstCache.set(key, inst);
    return inst;
  }

  /** Show `level`'s field (crossfading from whatever is on screen), or fade it out (null). */
  private showHex(level: number | null): void {
    if (!this.hexData) return;
    const top = this.hexDraws[this.hexDraws.length - 1];
    const showing = top && top.to === 1;
    if (level !== null && showing && top.level === level && top.gen === this.hexGen) return;
    if (level === null && !this.hexDraws.some((d) => d.to === 1)) return;
    const now = performance.now();
    const dur = (this.reducedMotion ? TRANSITION_REDUCED_MS : TRANSITION_MS) * this.debugTimeScale;
    for (const d of this.hexDraws) Object.assign(d, { from: hexAlpha(d, now).a, to: 0, start: now, dur });
    if (level !== null) {
      const inst = this.hexInstances(level);
      this.hexDraws.push({ level, gen: this.hexGen, data: inst.data, n: inst.n, glBuf: null, from: 0, to: 1, start: now, dur });
    }
    this.map?.triggerRepaint();
  }

  /** The cell under a point in the wide band, for the hover readout. */
  hexAt(lng: number, lat: number): { count: number; users: number } | null {
    const top = this.hexDraws[this.hexDraws.length - 1];
    if (!top || top.to !== 1 || !this.hexData || top.gen !== this.hexGen) return null;
    const h = this.hexData.hex[top.level];
    const g = hexGrid(top.level);
    let index = this.hexIndex.get(top.level);
    if (!index) {
      index = new Map();
      for (let i = 0; i < h.q.length; i++) index.set(hexKey(g, h.q[i], h.r[i]), i);
      this.hexIndex.set(top.level, index);
    }
    const [q, r] = hexCell(g, lonToMerc(lng), latToMerc(lat));
    const i = index.get(hexKey(g, q, r));
    return i === undefined ? null : { count: h.count[i], users: h.users[i] };
  }

  private onHexAdd(gl: WebGL2RenderingContext): void {
    const prog = link(gl, VS_HEX, FS_HEX);
    // Six triangles (centre, rim i, rim i+1); the third component is 0 at the centre, 1 on the rim.
    const geom: number[] = [];
    for (let i = 0; i < 6; i++) {
      const a0 = (i * Math.PI) / 3;
      const a1 = ((i + 1) * Math.PI) / 3;
      geom.push(0, 0, 0, Math.cos(a0), Math.sin(a0), 1, Math.cos(a1), Math.sin(a1), 1);
    }
    const buf = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(geom), gl.STATIC_DRAW);
    const vao = gl.createVertexArray()!;
    gl.bindVertexArray(vao);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 12, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 1, gl.FLOAT, false, 12, 8);
    for (const loc of [2, 3, 4]) {
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribDivisor(loc, 1);
    }
    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);
    this.hexGl = {
      prog,
      vao,
      geom: buf,
      uMatrix: gl.getUniformLocation(prog, "u_matrix"),
      uWrap: gl.getUniformLocation(prog, "u_wrap"),
      uAlpha: gl.getUniformLocation(prog, "u_alpha"),
    };
  }

  private onHexRemove(gl: WebGL2RenderingContext): void {
    for (const d of this.hexDraws) {
      if (d.glBuf) gl.deleteBuffer(d.glBuf);
      d.glBuf = null;
    }
    const h = this.hexGl;
    if (!h) return;
    gl.deleteProgram(h.prog);
    gl.deleteVertexArray(h.vao);
    gl.deleteBuffer(h.geom);
    this.hexGl = null;
  }

  private renderHexes(gl: WebGL2RenderingContext, options: maplibregl.CustomRenderMethodInput): void {
    const map = this.map;
    const hg = this.hexGl;
    if (!map || !hg || this.hexDraws.length === 0) return;
    const now = performance.now();
    // The field is only on screen at z ≤ 6, where an f32 copy of the matrix is exact enough.
    const m = Float32Array.from(options.defaultProjectionData.mainMatrix as unknown as ArrayLike<number>);
    const b = map.getBounds();
    // One draw per visible world copy (several at z0–2); cells near the seam overhang by R.
    const k0 = Math.floor(lonToMerc(b.getWest())) - 1;
    const k1 = Math.floor(lonToMerc(b.getEast()));
    gl.useProgram(hg.prog);
    gl.uniformMatrix4fv(hg.uMatrix, false, m);
    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.bindVertexArray(hg.vao);
    let animating = false;
    let drew = false;
    const kept: HexDraw[] = [];
    for (const d of this.hexDraws) {
      const { a, p } = hexAlpha(d, now);
      if (p < 1) animating = true;
      if (d.to === 0 && p >= 1) {
        if (d.glBuf) gl.deleteBuffer(d.glBuf);
        continue;
      }
      kept.push(d);
      if (d.reupload && d.glBuf) {
        gl.deleteBuffer(d.glBuf);
        d.glBuf = null;
      }
      d.reupload = false;
      if (a <= 0.003 || d.n === 0) continue;
      if (!d.glBuf) {
        d.glBuf = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, d.glBuf);
        gl.bufferData(gl.ARRAY_BUFFER, d.data, gl.STATIC_DRAW);
      }
      gl.bindBuffer(gl.ARRAY_BUFFER, d.glBuf);
      gl.vertexAttribPointer(2, 3, gl.FLOAT, false, HEX_STRIDE * 4, 0);
      gl.vertexAttribPointer(3, 4, gl.FLOAT, false, HEX_STRIDE * 4, 12);
      gl.vertexAttribPointer(4, 4, gl.FLOAT, false, HEX_STRIDE * 4, 28);
      gl.uniform1f(hg.uAlpha, a);
      for (let k = k0; k <= k1; k++) {
        gl.uniform1f(hg.uWrap, k);
        gl.drawArraysInstanced(gl.TRIANGLES, 0, 18, d.n);
      }
      drew = true;
    }
    this.hexDraws = kept;
    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);
    if (drew && !this.stats.firstDataFrameAt) {
      this.stats.firstDataFrameAt = now;
      performance.mark("ff:first-data-frame");
    }
    if (animating) map.triggerRepaint();
  }

  // ---- cluster bands: queries and transitions -----------------------------

  private liveNodes(): DNode[] {
    return this.nodes.filter((n) => !n.dying);
  }

  private fadeOutAll(): void {
    const now = performance.now();
    const dur = (this.reducedMotion ? TRANSITION_REDUCED_MS : TRANSITION_MS) * this.debugTimeScale;
    this.transition = { from: this.displayLevel, to: levelForZoom(this.map?.getZoom() ?? 0), start: now, crossedAt: this.levelCrossAt || now };
    for (const n of this.nodes) {
      if (n.dying) continue;
      const c = current(n, now);
      Object.assign(n, { fx: c.x, fy: c.y, fr: c.r, fa: c.a, tx: c.x, ty: c.y, tr: c.r * 0.6, ta: 0, start: now, dur, dying: true });
    }
    this.lastQuery = null;
    this.map?.triggerRepaint();
  }

  private scheduleQuery(): void {
    const map = this.map;
    if (!map || !this.index || this.gen !== this.client.gen) return;
    const level = levelForZoom(map.getZoom());
    if (level <= WIDE_MAX_LEVEL) return;
    const lq = this.lastQuery;
    if (lq && lq.gen === this.gen && lq.level === level && containsBBox(lq.bbox, this.viewBBox(0))) return;
    if (this.inflight) {
      this.pendingQuery = true;
      return;
    }
    void this.runQuery(level);
  }

  private async runQuery(level: number): Promise<void> {
    this.inflight = true;
    const bbox = this.viewBBox(QUERY_PAD);
    const gen = this.gen;
    const prevLevel = this.displayLevel;
    const prevNodes = this.nodesGen === gen ? this.liveNodes() : [];
    const prev =
      prevLevel !== null && prevLevel >= CLUSTER_MIN_LEVEL && prevNodes.length > 0
        ? { level: prevLevel, ids: Float64Array.from(prevNodes, (n) => n.id) }
        : null;
    let reply: { gen: number; result: QueryResult };
    try {
      reply = await this.client.query(bbox, level, prev);
    } catch (e) {
      this.inflight = false;
      console.warn("camera query failed", e);
      return;
    }
    this.inflight = false;
    const map = this.map;
    const wanted = !!map && levelForZoom(map.getZoom()) === level && reply.gen === this.gen && gen === this.gen;
    if (wanted) {
      this.apply(reply.result, prev ? prevNodes : [], prevLevel);
      this.lastQuery = { level, bbox, gen };
    }
    if (this.pendingQuery || !wanted) {
      this.pendingQuery = false;
      this.scheduleQuery();
    }
  }

  private makeNode(r: QueryResult, i: number, scale: number, maxCount: number): DNode {
    const count = r.count[i];
    const leaf = r.leaf[i];
    const radius = count > 1 ? nodeRadius(count, scale) : leaf >= this.points.count ? POINT_RADIUS_PX + 0.8 : POINT_RADIUS_PX;
    return {
      id: r.ids[i],
      leaf,
      count,
      users: r.users[i],
      t: count > 1 ? densityT(count, maxCount) : 0,
      fx: r.x[i],
      fy: r.y[i],
      fr: radius,
      fa: 1,
      tx: r.x[i],
      ty: r.y[i],
      tr: radius,
      ta: 1,
      start: 0,
      dur: 0,
      dying: false,
    };
  }

  /** Swap in a new node set, animating from wherever each node currently is. */
  private apply(res: QueryResult, prevNodes: DNode[], prevLevel: number | null): void {
    const now = performance.now();
    const reduced = this.reducedMotion;
    const ts = this.debugTimeScale;
    const dur = (reduced ? TRANSITION_REDUCED_MS : TRANSITION_MS) * ts;
    const fadeInMs = FADE_IN_MS * ts;
    const scale = this.scaleFor(res.level);
    const maxCount = this.maxFor(res.level);
    if (prevLevel !== res.level) this.transition = { from: prevLevel, to: res.level, start: now, crossedAt: this.levelCrossAt || now };
    const next: DNode[] = [];
    for (let i = 0; i < res.ids.length; i++) next.push(this.makeNode(res, i, scale, maxCount));

    // Freeze every existing node at its current on-screen state.
    const live = new Map<number, { n: DNode; x: number; y: number; r: number; a: number }>();
    const dying: DNode[] = [];
    for (const n of this.nodes) {
      const c = current(n, now);
      if (n.dying) {
        if (c.p < 1) dying.push(n);
        continue;
      }
      live.set(n.id, { n, x: c.x, y: c.y, r: c.r, a: c.a });
    }
    const sameBuild = this.nodesGen === this.gen;
    const fromState = (n: DNode, x: number, y: number, r: number, a: number, d: number) =>
      Object.assign(n, { fx: x, fy: y, fr: r, fa: a, start: now, dur: d });
    const fadeIn = (n: DNode, d: number, grow = 1) => fromState(n, n.tx, n.ty, n.tr * grow, 0, d);
    const fadeOut = (n: DNode, x: number, y: number, r: number, a: number, tx: number, ty: number, tr: number, d: number) =>
      dying.push(Object.assign(n, { fx: x, fy: y, fr: r, fa: a, tx, ty, tr, ta: 0, start: now, dur: d, dying: true }));

    const consumed = new Set<number>();
    if (sameBuild && res.dir === "in" && res.anc && !reduced) {
      // Zooming in: children emerge from the parent they split out of.
      res.anc.forEach((pid, i) => {
        const n = next[i];
        const parent = live.get(pid);
        if (!parent) return fadeIn(n, dur, 0.6);
        consumed.add(pid);
        if (pid === n.id) fromState(n, parent.x, parent.y, parent.r, parent.a, dur);
        else fromState(n, parent.x, parent.y, Math.min(parent.r, n.tr * 1.4), Math.max(0.35, parent.a), dur);
      });
      for (const [id, o] of live) {
        if (consumed.has(id)) continue;
        fadeOut(o.n, o.x, o.y, o.r, o.a, o.x, o.y, o.r, dur);
      }
    } else if (sameBuild && res.dir === "out" && res.anc && !reduced) {
      // Zooming out: nodes converge on the cluster they merge into, which grows in.
      const byId = new Map(next.map((n) => [n.id, n]));
      const continued = new Set<number>();
      const absorbing = new Set<number>();
      prevNodes.forEach((old, j) => {
        const o = live.get(old.id);
        if (!o) return;
        const target = byId.get(res.anc![j]);
        if (!target) return fadeOut(o.n, o.x, o.y, o.r, o.a, o.x, o.y, o.r * 0.6, dur);
        if (target.id === old.id) {
          fromState(target, o.x, o.y, o.r, o.a, dur);
          continued.add(target.id);
        } else {
          fadeOut(o.n, o.x, o.y, o.r, o.a, target.tx, target.ty, Math.min(o.r, target.tr * 0.7), dur);
          absorbing.add(target.id);
        }
      });
      for (const n of next) {
        if (continued.has(n.id)) continue;
        fadeIn(n, dur, absorbing.has(n.id) ? 0.55 : 0.7);
      }
    } else {
      // Same level (a pan, new data, new filters) or reduced motion: continue matching
      // nodes, crossfade the rest in place.
      const fresh = prevLevel !== res.level || !sameBuild;
      const d = fresh ? dur : fadeInMs;
      for (const n of next) {
        const o = sameBuild && prevLevel === res.level ? live.get(n.id) : undefined;
        if (o) {
          fromState(n, o.x, o.y, o.r, o.a, fadeInMs);
          consumed.add(n.id);
        } else {
          fadeIn(n, d, fresh && !reduced ? 0.7 : 1);
        }
      }
      for (const [id, o] of live) {
        if (consumed.has(id)) continue;
        fadeOut(o.n, o.x, o.y, o.r, o.a, o.x, o.y, o.r, d);
      }
    }

    this.nodes = [...next, ...dying];
    this.nodesGen = this.gen;
    this.displayLevel = res.level;
    this.map?.triggerRepaint();
  }

  // ---- counts --------------------------------------------------------------

  private scheduleCounts(now: boolean): void {
    if (!this.map || !this.hexData) return;
    const run = () => {
      this.countsTimer = null;
      this.countsDue = performance.now() + COUNT_THROTTLE_MS;
      const gen = this.hexGen;
      void this.client
        .counts(this.viewBBox(0))
        .then((r) => {
          if (r.gen === gen && gen === this.hexGen) this.events.onCounts(r.result);
        })
        .catch(() => {});
    };
    if (now) {
      if (this.countsTimer) window.clearTimeout(this.countsTimer);
      run();
      return;
    }
    if (this.countsTimer) return;
    this.countsTimer = window.setTimeout(run, Math.max(0, this.countsDue - performance.now()));
  }

  // ---- motion extras -------------------------------------------------------

  private startPulse(): void {
    if (this.reducedMotion || !this.map) return;
    const level = levelForZoom(this.map.getZoom());
    if (level < CLUSTER_MIN_LEVEL || level > NEAR_MAX_LEVEL) return;
    const cands = this.frame
      .filter((f) => f.n.count > 1 && !f.n.dying && f.n.t >= PULSE_MIN_T && f.a > 0.9)
      .sort((a, b) => b.n.count - a.n.count)
      .slice(0, PULSE_MAX_NODES);
    if (cands.length === 0) return;
    this.pulse = { start: performance.now(), ids: new Set(cands.map((c) => c.n.id)) };
    this.stats.pulses++;
    this.map.triggerRepaint();
  }

  // ---- picking -------------------------------------------------------------

  private pick(x: number, y: number, slop: number): FrameNode | null {
    let best: FrameNode | null = null;
    let bestD = Infinity;
    for (const f of this.frame) {
      if (f.n.dying || f.a < 0.5) continue;
      const d = Math.hypot(f.sx - x, f.sy - y);
      if (d <= f.r + slop && d < bestD) {
        best = f;
        bestD = d;
      }
    }
    return best;
  }

  /** Whether a pointer at this screen position is over something clickable. */
  hoverTest(x: number, y: number): boolean {
    return this.pick(x, y, 4) !== null;
  }

  /** Handle a click in view mode. Returns true when the click hit the camera layer. */
  handleClick(x: number, y: number, touch: boolean): boolean {
    const map = this.map;
    if (!map) return false;
    const hit = this.pick(x, y, touch ? 12 : 5);
    if (!hit) {
      if (this.spider) {
        this.spider = null;
        map.triggerRepaint();
      }
      return false;
    }
    const n = hit.n;
    if (n.count > 1) {
      const gen = this.gen;
      void this.client.expand(n.id).then((r) => {
        if (r.gen !== gen || !r.result || !this.map) return;
        const [w, s, e, nn] = r.result.bounds;
        const cam = this.map.cameraForBounds([[w, s], [e, nn]], { padding: 72 });
        const zoom = Math.min(19, Math.max(cam?.zoom ?? 0, r.result.zoom));
        const center = cam?.center ?? maplibregl.LngLat.convert([(w + e) / 2, (s + nn) / 2]);
        this.map.easeTo({ center, zoom, duration: this.reducedMotion ? 0 : 650 });
      });
      return true;
    }
    const level = levelForZoom(map.getZoom());
    const inSpider = this.spider?.leaves.includes(n.leaf);
    if (!inSpider && level >= SPIDER_MIN_LEVEL) {
      const group = this.frame.filter(
        (f) => !f.n.dying && f.n.count === 1 && Math.hypot(f.sx - hit.sx, f.sy - hit.sy) <= SPIDER_PICK_PX,
      );
      if (group.length >= 2) {
        this.spider = { level, x: n.tx, y: n.ty, leaves: group.map((f) => f.n.leaf) };
        map.triggerRepaint();
        return true;
      }
    }
    if (n.leaf >= this.points.count) this.events.onPickSubmission(this.subs[n.leaf - this.points.count].id);
    else this.events.onPickCamera(n.leaf);
    return true;
  }

  // ---- rendering -----------------------------------------------------------

  private conesFor(leaf: number): Cone[] {
    let c = this.coneCache.get(leaf);
    if (c) return c;
    if (leaf >= this.points.count) {
      const d = this.subs[leaf - this.points.count]?.direction;
      c = d === null || d === undefined ? [] : [{ bearing: d, width: DEFAULT_FOV }];
    } else {
      const raw = this.points.directions.get(leaf);
      c = raw ? parseDirectionValue(raw).slice(0, MAX_CONES) : [];
    }
    this.coneCache.set(leaf, c);
    return c;
  }

  private resizeCanvas(W: number, H: number): number {
    const dpr = window.devicePixelRatio || 1;
    const c = this.canvas!;
    const w = Math.round(W * dpr);
    const h = Math.round(H * dpr);
    if (c.width !== w || c.height !== h) {
      c.width = w;
      c.height = h;
      c.style.width = `${W}px`;
      c.style.height = `${H}px`;
    }
    return dpr;
  }

  private projector(options: maplibregl.CustomRenderMethodInput, W: number, H: number): (x: number, y: number) => [number, number] | null {
    const m = options.defaultProjectionData?.mainMatrix as unknown as ArrayLike<number> | undefined;
    if (!this.matrixChecked && m) {
      // mainMatrix maps mercator [0, 1] to clip space. Only trust it at f64 precision: at z19
      // an f32 matrix is off by several pixels. Verify against map.project once.
      this.matrixChecked = true;
      const c = this.map!.getCenter();
      const p = this.map!.project(c);
      const x = lonToMerc(c.lng);
      const y = 0.5 - Math.log(Math.tan(Math.PI / 4 + (c.lat * Math.PI) / 360)) / (2 * Math.PI);
      const w = m[3] * x + m[7] * y + m[15];
      const sx = ((m[0] * x + m[4] * y + m[12]) / w + 1) * 0.5 * W;
      const sy = (1 - (m[1] * x + m[5] * y + m[13]) / w) * 0.5 * H;
      this.useMatrix = m instanceof Float64Array && Math.hypot(sx - p.x, sy - p.y) < 0.5;
      this.stats.projection = `${m.constructor.name}, err ${Math.hypot(sx - p.x, sy - p.y).toFixed(3)} px → ${this.useMatrix ? "matrix" : "map.project"}`;
    }
    if (this.useMatrix && m) {
      return (x, y) => {
        const w = m[3] * x + m[7] * y + m[15];
        if (w <= 1e-9) return null;
        return [((m[0] * x + m[4] * y + m[12]) / w + 1) * 0.5 * W, (1 - (m[1] * x + m[5] * y + m[13]) / w) * 0.5 * H];
      };
    }
    const map = this.map!;
    return (x, y) => {
      const p = map.project([mercToLon(x), mercToLat(y)]);
      return [p.x, p.y];
    };
  }

  render(gl: WebGL2RenderingContext, options: maplibregl.CustomRenderMethodInput): void {
    const map = this.map;
    const g = this.gl;
    if (!map || !g || !this.canvas || !this.ctx) return;
    const now = performance.now();
    const mapCanvas = map.getCanvas();
    const W = mapCanvas.clientWidth;
    const H = mapCanvas.clientHeight;
    const dpr = this.resizeCanvas(W, H);
    const zoom = map.getZoom();
    const level = levelForZoom(zoom);
    const bearing = (map.getBearing() * Math.PI) / 180;
    const cx = lonToMerc(map.getCenter().lng);
    const project = this.projector(options, W, H);
    let animating = false;

    // Advance animations; drop finished fade-outs.
    const kept: DNode[] = [];
    const frame: FrameNode[] = [];
    let nodesAnimating = false;
    let spiderCenter: [number, number] | null = null;
    if (this.spider) {
      const sx = this.spider.x + Math.round(cx - this.spider.x);
      spiderCenter = project(sx, this.spider.y);
    }
    const spiderIndex = new Map<number, number>();
    this.spider?.leaves.forEach((leaf, i) => spiderIndex.set(leaf, i));
    for (const n of this.nodes) {
      const c = current(n, now);
      if (n.dying && c.p >= 1) continue;
      kept.push(n);
      if (c.p < 1) nodesAnimating = true;
      if (c.a <= 0.004) continue;
      const x = c.x + Math.round(cx - c.x); // nearest world copy
      const s = project(x, c.y);
      if (!s) continue;
      let [sx, sy] = s;
      const legIdx = n.count === 1 ? spiderIndex.get(n.leaf) : undefined;
      if (legIdx !== undefined && spiderCenter) {
        const k = this.spider!.leaves.length;
        const ang = -Math.PI / 2 + (2 * Math.PI * legIdx) / k;
        const rad = k <= 8 ? Math.max(24, 8 + 5 * k) : 18 + 4 * legIdx;
        [sx, sy] = [spiderCenter[0] + Math.cos(ang) * rad, spiderCenter[1] + Math.sin(ang) * rad];
      }
      const margin = Math.max(c.r * HALO_SCALE, CONE_RADIUS_PX * 1.3);
      if (sx < -margin || sy < -margin || sx > W + margin || sy > H + margin) continue;
      frame.push({ n, sx, sy, r: c.r, a: c.a });
    }
    this.nodes = kept;
    this.frame = frame;
    this.stats.drawn = frame.length;
    if (nodesAnimating) animating = true;
    else if (this.transition) {
      const t = this.transition;
      this.stats.transitions.push({ from: t.from, to: t.to, anim: now - t.start, total: now - t.crossedAt });
      if (this.stats.transitions.length > 50) this.stats.transitions.shift();
      this.transition = null;
    }

    // Pulse: a few slow breaths on the densest clusters after the view settles.
    let pulseMult: ((id: number) => number) | null = null;
    if (this.pulse) {
      const el = now - this.pulse.start;
      if (el >= PULSE_PERIOD_MS * PULSE_CYCLES) this.pulse = null;
      else {
        animating = true;
        const s = Math.sin((Math.PI * el) / PULSE_PERIOD_MS) ** 2;
        const ids = this.pulse.ids;
        pulseMult = (id) => (ids.has(id) ? 1 + 0.9 * s : 1);
      }
    }

    // Fill instance batches.
    const halo = g.halo.batch;
    const core = g.node.batch;
    const cone = g.cone.batch;
    halo.reset();
    core.reset();
    cone.reset();
    const drawCones = this.cones && level >= CONE_MIN_LEVEL;
    const coneR = CONE_RADIUS_PX * coneScale(zoom);
    const pal = this.pal;
    const { accent, state, ramp } = pal;
    for (const f of frame) {
      const { n, sx, sy, r, a } = f;
      if (n.count > 1) {
        const pm = pulseMult ? pulseMult(n.id) : 1;
        const fill = ramp(0.18 + 0.82 * n.t);
        const edge = ramp(Math.min(1, pal.clusterEdge + 0.8 * n.t));
        if (pal.halos) {
          const hc = ramp(Math.max(0.5, n.t));
          halo.push(sx, sy, r * HALO_SCALE, hc[0], hc[1], hc[2], HALO_MAX_INTENSITY * (0.3 + 0.7 * n.t) * a * pm);
        }
        const fa = 0.9 * a;
        core.push(sx, sy, r, fill[0] * fa, fill[1] * fa, fill[2] * fa, fa, edge[0] * a, edge[1] * a, edge[2] * a, a, 0, n.users > 0 ? a : 0);
      } else {
        const user = n.leaf >= this.points.count;
        const flock = !user && pointKind(this.points.flags[n.leaf]) === "flock";
        if (drawCones) {
          const col = user ? state : accent;
          for (const c of this.conesFor(n.leaf)) {
            cone.push(sx, sy, coneR, (c.bearing * Math.PI) / 180 - bearing, (c.width * Math.PI) / 360, col[0], col[1], col[2], a * 0.9);
          }
        }
        const col = user ? state : accent;
        if (pal.halos) halo.push(sx, sy, r * 3.2, col[0], col[1], col[2], (user ? 0.3 : flock ? 0.22 : 0.14) * a);
        if (user || flock) {
          const e = user ? pal.stateEdge : pal.accentEdge;
          core.push(sx, sy, r, col[0] * a, col[1] * a, col[2] * a, a, e[0] * a, e[1] * a, e[2] * a, a, 0, 0);
        } else {
          const fa = pal.ringFillAlpha * a;
          const inner = pal.ringFill;
          core.push(sx, sy, r, inner[0] * fa, inner[1] * fa, inner[2] * fa, fa, col[0] * a, col[1] * a, col[2] * a, a, 1, 0);
        }
      }
    }

    // Draw: halos additively (bright cores, soft falloff), then cones and cores normally.
    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    const aa = 0.55 / dpr + 0.2;
    if (halo.n > 0) {
      gl.useProgram(g.halo.prog);
      gl.uniform2f(g.halo.uViewport, W, H);
      gl.blendFuncSeparate(gl.ONE, gl.ONE, gl.ZERO, gl.ONE);
      halo.draw(gl);
    }
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    if (cone.n > 0) {
      gl.useProgram(g.cone.prog);
      gl.uniform2f(g.cone.uViewport, W, H);
      gl.uniform1f(g.cone.uAa, aa);
      cone.draw(gl);
    }
    if (core.n > 0) {
      gl.useProgram(g.node.prog);
      gl.uniform2f(g.node.uViewport, W, H);
      gl.uniform1f(g.node.uAa, aa);
      gl.uniform3f(g.node.uState, state[0], state[1], state[2]);
      core.draw(gl);
    }
    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    // Labels, leaders and the sweep on the 2D overlay.
    const ctx = this.ctx;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);
    if (level >= CLUSTER_MIN_LEVEL && level <= NEAR_MAX_LEVEL) this.drawLabels(ctx, frame, W, H);
    if (spiderCenter && this.spider) this.drawSpider(ctx, frame, spiderCenter);
    if (this.sweep) {
      const el = now - this.sweep.start;
      if (el >= SWEEP_MS) this.sweep = null;
      else {
        animating = true;
        this.drawSweep(ctx, easeOutCubic(el / SWEEP_MS), W, H);
      }
    }
    if (!this.stats.firstDataFrameAt && frame.length > 0) {
      this.stats.firstDataFrameAt = now;
      performance.mark("ff:first-data-frame");
    }
    if (animating) map.triggerRepaint();
  }

  private drawLabels(ctx: CanvasRenderingContext2D, frame: FrameNode[], W: number, H: number): void {
    const reqs: LabelRequest[] = [];
    const obstacles: LabelRequest[] = [];
    const byId = new Map<number, FrameNode>();
    const texts = new Map<number, { main: string; extra: string }>();
    for (const f of frame) {
      if (f.n.count <= 1) continue;
      obstacles.push({ id: f.n.id, x: f.sx, y: f.sy, r: f.r, w: 0, h: 0, priority: 0 });
      if (f.a < 0.4 || f.n.dying) continue;
      const verified = f.n.count - f.n.users;
      const main = formatCount(verified);
      const extra = f.n.users > 0 ? `+${f.n.users}` : "";
      texts.set(f.n.id, { main, extra });
      byId.set(f.n.id, f);
      reqs.push({
        id: f.n.id,
        x: f.sx,
        y: f.sy,
        r: f.r,
        w: (main.length + extra.length) * this.charW + 2 * LABEL_PAD,
        h: LABEL_H,
        priority: f.n.count,
      });
    }
    const placed = placeLabels(reqs, W, H, obstacles);
    this.stats.labels = {
      requested: reqs.length,
      placed: placed.length,
      inside: placed.filter((p) => p.inside).length,
      overlaps: import.meta.env.DEV ? anyOverlap(placed) : false,
    };
    ctx.font = `600 ${LABEL_FONT_PX}px ${MONO}`;
    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    const ramp = this.pal.ramp;
    const accentLine = rgbStr(ramp(0.7), 0.55);
    for (const p of placed) {
      const f = byId.get(p.id)!;
      const t = texts.get(p.id)!;
      ctx.globalAlpha = f.a;
      const tx = p.x + LABEL_PAD;
      const ty = p.y + p.h / 2 + 0.5;
      if (p.inside) {
        const fill = ramp(0.18 + 0.82 * f.n.t);
        ctx.fillStyle = relativeLuminance(fill) > 0.3 ? "rgba(3, 10, 16, 0.95)" : "rgba(236, 250, 255, 0.96)";
        ctx.fillText(t.main, tx, ty);
      } else {
        ctx.strokeStyle = accentLine;
        ctx.lineWidth = 1;
        ctx.beginPath();
        const bx = p.lx > p.x + p.w / 2 ? p.x + p.w : p.x;
        const by = p.ly > p.y + p.h / 2 ? p.y + p.h : p.y;
        ctx.moveTo(p.lx, p.ly);
        ctx.lineTo(bx, by);
        ctx.stroke();
        ctx.fillStyle = "rgba(4, 8, 13, 0.86)";
        ctx.beginPath();
        ctx.roundRect(p.x, p.y, p.w, p.h, 2);
        ctx.fill();
        ctx.strokeStyle = rgbStr(ramp(0.6), 0.4);
        ctx.strokeRect(Math.round(p.x) + 0.5, Math.round(p.y) + 0.5, Math.round(p.w) - 1, Math.round(p.h) - 1);
        ctx.fillStyle = "rgba(226, 247, 255, 0.97)";
        ctx.fillText(t.main, tx, ty);
      }
      if (t.extra) {
        ctx.fillStyle = rgbStr(this.pal.state, 1);
        ctx.fillText(t.extra, tx + t.main.length * this.charW, ty);
      }
    }
    ctx.globalAlpha = 1;
  }

  private drawSpider(ctx: CanvasRenderingContext2D, frame: FrameNode[], center: [number, number]): void {
    const legs = new Set(this.spider!.leaves);
    ctx.strokeStyle = rgbStr(this.pal.ramp(0.75), 0.6);
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (const f of frame) {
      if (f.n.count !== 1 || !legs.has(f.n.leaf)) continue;
      ctx.moveTo(center[0], center[1]);
      ctx.lineTo(f.sx, f.sy);
    }
    ctx.stroke();
    ctx.fillStyle = rgbStr(this.pal.ramp(0.9), 0.9);
    ctx.beginPath();
    ctx.arc(center[0], center[1], 2, 0, Math.PI * 2);
    ctx.fill();
  }

  private drawSweep(ctx: CanvasRenderingContext2D, p: number, W: number, H: number): void {
    const x = -24 + p * (W + 48);
    const { sweepShade, accent, ramp } = this.pal;
    ctx.fillStyle = sweepShade;
    ctx.fillRect(Math.max(0, x), 0, W - Math.max(0, x), H);
    const trail = ctx.createLinearGradient(x - 90, 0, x, 0);
    trail.addColorStop(0, rgbStr(accent, 0));
    trail.addColorStop(1, rgbStr(accent, 0.16));
    ctx.fillStyle = trail;
    ctx.fillRect(x - 90, 0, 90, H);
    ctx.fillStyle = rgbStr(ramp(this.pal.scheme === "light" ? 0.7 : 0.95), 0.85);
    ctx.fillRect(Math.round(x), 0, 1, H);
  }
}
