/**
 * Flat-top hexagonal binning in Web Mercator (x, y in [0, 1]) for the wide-band density
 * field. The number of hex columns around the world is even, so the grid wraps exactly at
 * the antimeridian: a cell straddling ±180° collects points from both sides instead of
 * being split into two half-cells.
 */
import { HEX_RADIUS_PX } from "./lod";

export interface HexGrid {
  level: number;
  /** Hex columns around the world (even). */
  cols: number;
  /** Circumradius in mercator units. */
  R: number;
}

const SQRT3 = Math.sqrt(3);

export function hexGrid(level: number, radiusPx = HEX_RADIUS_PX): HexGrid {
  const worldPx = 512 * 2 ** level;
  const cols = Math.max(2, 2 * Math.round(worldPx / (1.5 * radiusPx) / 2));
  return { level, cols, R: 1 / (1.5 * cols) };
}

const mod = (a: number, n: number) => ((a % n) + n) % n;

/** Axial (q, r) of the cell containing mercator (x, y), with q wrapped into [0, cols). */
export function hexCell(g: HexGrid, x: number, y: number): [number, number] {
  const qf = ((2 / 3) * x) / g.R;
  const rf = ((-1 / 3) * x + (SQRT3 / 3) * y) / g.R;
  const sf = -qf - rf;
  let q = Math.round(qf);
  let r = Math.round(rf);
  const s = Math.round(sf);
  const dq = Math.abs(q - qf);
  const dr = Math.abs(r - rf);
  const ds = Math.abs(s - sf);
  if (dq > dr && dq > ds) q = -r - s;
  else if (dr > ds) r = -q - s;
  // Shifting q by `cols` moves x by exactly one world; r shifts by cols/2 to keep y.
  const qw = mod(q, g.cols);
  return [qw, r + (q - qw) / 2];
}

/** Unique integer key for a wrapped cell. */
export function hexKey(g: HexGrid, q: number, r: number): number {
  return q * (4 * g.cols) + (r + 2 * g.cols);
}

export function hexCenter(g: HexGrid, q: number, r: number): [number, number] {
  return [g.R * 1.5 * q, g.R * SQRT3 * (r + q / 2)];
}

export function mercToLon(x: number): number {
  return (x - 0.5) * 360;
}

export function mercToLat(y: number): number {
  const y2 = ((180 - y * 360) * Math.PI) / 180;
  return (360 * Math.atan(Math.exp(y2))) / Math.PI - 90;
}

export function lonToMerc(lon: number): number {
  return lon / 360 + 0.5;
}

export function latToMerc(lat: number): number {
  const s = Math.sin((lat * Math.PI) / 180);
  const y = 0.5 - (0.25 * Math.log((1 + s) / (1 - s))) / Math.PI;
  return y < 0 ? 0 : y > 1 ? 1 : y;
}

/** Closed polygon ring (lon, lat) for a cell. Longitudes may run past ±180 at the seam. */
export function hexRing(g: HexGrid, q: number, r: number): [number, number][] {
  const [cx, cy] = hexCenter(g, q, r);
  const ring: [number, number][] = [];
  for (let i = 0; i <= 6; i++) {
    const a = (Math.PI / 3) * (i % 6);
    const y = Math.max(0, Math.min(1, cy + g.R * Math.sin(a)));
    ring.push([mercToLon(cx + g.R * Math.cos(a)), mercToLat(y)]);
  }
  return ring;
}

/** Ground width of a cell (flat side to flat side is √3·R; point to point is 2R) in km. */
export function hexWidthKm(g: HexGrid, latDeg: number): number {
  return 2 * g.R * 40_075 * Math.cos((latDeg * Math.PI) / 180);
}
