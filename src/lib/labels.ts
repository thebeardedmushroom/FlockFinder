/**
 * Count-label placement with collision avoidance. Labels are placed greedily, most
 * important first: inside the node when the text fits, otherwise as a callout at one of
 * four diagonal positions joined to the node by a hairline. A label that fits nowhere is
 * dropped for this frame rather than drawn over another; overlapping numbers are the
 * failure mode this exists to prevent.
 */

export interface LabelRequest {
  id: number;
  /** Node centre and core radius, CSS px. */
  x: number;
  y: number;
  r: number;
  /** Text box size, CSS px. */
  w: number;
  h: number;
  /** Higher goes first (the count). */
  priority: number;
}

export interface PlacedLabel {
  id: number;
  /** Top-left of the text box. */
  x: number;
  y: number;
  w: number;
  h: number;
  inside: boolean;
  /** Leader start on the node's edge (callouts only). */
  lx: number;
  ly: number;
}

interface Box {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  owner: number;
}

const CELL = 64;
/** Space kept between the text and the node edge for inside labels. */
const INSIDE_PAD = 3;
const CALLOUT_GAP = 5;
const MARGIN = 2;

class BoxGrid {
  private cells = new Map<number, Box[]>();

  private *keys(b: Box): Generator<number> {
    for (let cx = Math.floor(b.x0 / CELL); cx <= Math.floor(b.x1 / CELL); cx++) {
      for (let cy = Math.floor(b.y0 / CELL); cy <= Math.floor(b.y1 / CELL); cy++) yield cx * 100_003 + cy;
    }
  }

  add(b: Box): void {
    for (const k of this.keys(b)) {
      const list = this.cells.get(k);
      if (list) list.push(b);
      else this.cells.set(k, [b]);
    }
  }

  hits(b: Box, ignoreOwner: number): boolean {
    for (const k of this.keys(b)) {
      for (const o of this.cells.get(k) ?? []) {
        if (o.owner === ignoreOwner) continue;
        if (b.x0 < o.x1 && b.x1 > o.x0 && b.y0 < o.y1 && b.y1 > o.y0) return true;
      }
    }
    return false;
  }
}

/**
 * @param obstacles node cores other labels must not cover (their own label may).
 */
export function placeLabels(requests: LabelRequest[], width: number, height: number, obstacles: LabelRequest[] = []): PlacedLabel[] {
  const labels = new BoxGrid();
  const cores = new BoxGrid();
  for (const o of obstacles) cores.add({ x0: o.x - o.r, y0: o.y - o.r, x1: o.x + o.r, y1: o.y + o.r, owner: o.id });

  const out: PlacedLabel[] = [];
  const onScreen = (b: Box) => b.x0 >= MARGIN && b.y0 >= MARGIN && b.x1 <= width - MARGIN && b.y1 <= height - MARGIN;
  const sorted = [...requests].sort((a, b) => b.priority - a.priority);
  for (const q of sorted) {
    // Inside the core, when the text fits in the inscribed square with padding.
    if (q.w + 2 * INSIDE_PAD <= 2 * q.r * 0.92 && q.h + 2 * INSIDE_PAD <= 2 * q.r * 0.92) {
      const b: Box = { x0: q.x - q.w / 2, y0: q.y - q.h / 2, x1: q.x + q.w / 2, y1: q.y + q.h / 2, owner: q.id };
      if (onScreen(b) && !labels.hits(b, -1)) {
        labels.add(b);
        out.push({ id: q.id, x: b.x0, y: b.y0, w: q.w, h: q.h, inside: true, lx: q.x, ly: q.y });
        continue;
      }
    }
    // Callouts: NE, NW, SE, SW.
    const d = q.r * Math.SQRT1_2;
    for (const [sx, sy] of [
      [1, -1],
      [-1, -1],
      [1, 1],
      [-1, 1],
    ] as const) {
      const ax = q.x + sx * (d + CALLOUT_GAP);
      const ay = q.y + sy * (d + CALLOUT_GAP);
      const x0 = sx > 0 ? ax : ax - q.w;
      const y0 = sy > 0 ? ay : ay - q.h;
      const b: Box = { x0, y0, x1: x0 + q.w, y1: y0 + q.h, owner: q.id };
      if (!onScreen(b) || labels.hits(b, -1) || cores.hits(b, q.id)) continue;
      labels.add(b);
      out.push({ id: q.id, x: x0, y: y0, w: q.w, h: q.h, inside: false, lx: q.x + sx * d, ly: q.y + sy * d });
      break;
    }
  }
  return out;
}

/** True if any two placed labels overlap (test helper, also used by dev assertions). */
export function anyOverlap(placed: PlacedLabel[]): boolean {
  for (let i = 0; i < placed.length; i++) {
    for (let j = i + 1; j < placed.length; j++) {
      const a = placed[i];
      const b = placed[j];
      if (a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y) return true;
    }
  }
  return false;
}
