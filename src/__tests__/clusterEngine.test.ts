import { describe, expect, it } from "vitest";
import { decodePoints, encodePoints, pointKey, type EncodableCamera } from "../lib/cameraData";
import { ClusterEngine, type BBoxTuple, type EngineFilter } from "../lib/clusterEngine";
import { hexCell, hexCenter, hexGrid, lonToMerc } from "../lib/hexbin";
import { CLUSTER_MIN_LEVEL, LEAF_LEVEL, NEAR_MAX_LEVEL, WIDE_MAX_LEVEL } from "../lib/lod";

const ALL: EngineFilter = { flock: true, alpr: true, user: true, operator: "" };
const WORLD: BBoxTuple = [-180, -85, 180, 85];

function rng(seed: number) {
  let s = seed;
  return () => ((s = (s * 16807) % 2147483647) / 2147483647);
}

/** Metro-like blobs plus scattered rural cameras, a few co-located, some near ±180°. */
function synthetic(n: number): EncodableCamera[] {
  const r = rng(42);
  const metros: [number, number, number][] = [
    [-84.39, 33.75, 0.35],
    [-104.99, 39.74, 0.3],
    [-118.24, 34.05, 0.5],
    [179.7, -16.6, 0.4], // straddles the antimeridian (Fiji)
  ];
  const cams: EncodableCamera[] = [];
  for (let i = 0; i < n; i++) {
    let lon: number;
    let lat: number;
    if (i % 5 === 0) {
      lon = -125 + r() * 58;
      lat = 25 + r() * 24;
    } else {
      const [mx, my, s] = metros[i % metros.length];
      lon = mx + (r() - 0.5) * s * 2;
      lat = my + (r() - 0.5) * s * 2;
      if (lon > 180) lon -= 360;
    }
    cams.push({
      osm_type: i % 97 === 0 ? "way" : "node",
      osm_id: 1_000_000 + i,
      lat,
      lon,
      category: i % 3 === 0 ? "alpr" : "flock",
      stale: i % 50 === 0,
      operator: i % 4 === 0 ? "Atlanta Police Department" : i % 7 === 0 ? "Home Depot" : null,
      direction: i % 2 === 0 ? String((i * 37) % 360) : null,
    });
  }
  // Five cameras on one pole.
  for (let k = 0; k < 5; k++) {
    cams.push({ osm_type: "node", osm_id: 9_000 + k, lat: 40.0001, lon: -100.0001, category: "flock", stale: false, operator: null, direction: null });
  }
  return cams;
}

function setup(n = 6000, users = 3) {
  const cams = synthetic(n);
  const points = decodePoints(encodePoints(cams));
  const engine = new ClusterEngine();
  const u = Array.from({ length: users }, (_, i) => ({ lon: -84.39 + i * 0.001, lat: 33.75, operator: i === 0 ? "Atlanta Police Department" : null }));
  engine.setData(points, u);
  return { cams, points, engine, users: u };
}

function countedCams(cams: EncodableCamera[], f: EngineFilter) {
  return cams.filter((c) => {
    if (c.stale) return false;
    if (c.category === "flock" ? !f.flock : !f.alpr) return false;
    return !f.operator || (c.operator ?? "").toLowerCase().includes(f.operator.toLowerCase());
  }).length;
}

describe("snapshot codec", () => {
  it("round-trips the binary layout", () => {
    const cams = synthetic(300);
    const p = decodePoints(encodePoints(cams));
    expect(p.count).toBe(cams.length);
    expect(p.lon[10]).toBe(cams[10].lon);
    expect(pointKey(p, 0)).toBe("way/1000000");
    expect(p.directions.get(2)).toBe(cams[2].direction);
    expect(p.operators).toContain("Home Depot");
  });
});

describe("cluster engine", () => {
  it("counts every included camera exactly once at every level", () => {
    const { engine, cams } = setup();
    const b = engine.build(ALL);
    const expected = countedCams(cams, ALL) + 3;
    expect(b.included).toBe(expected);
    for (let L = CLUSTER_MIN_LEVEL; L <= LEAF_LEVEL + 2; L++) {
      const q = engine.query(WORLD, L, null);
      const sum = q.count.reduce((a, c) => a + c, 0);
      expect(sum, `level ${L}`).toBe(expected);
    }
    for (const h of b.hex) expect(h.count.reduce((a, c) => a + c, 0)).toBe(expected);
  });

  it("cluster counts at low zoom equal the individual cameras at high zoom in the same area", () => {
    const { engine } = setup();
    engine.build(ALL);
    const mid = engine.query(WORLD, 7, null);
    // For each cluster, its leaves at the leaf level are exactly the cameras it counts.
    const leafLevel = engine.query(WORLD, LEAF_LEVEL, null);
    const perAncestor = new Map<number, number>();
    for (const id of leafLevel.ids) {
      const a = engine.ancestor(id, LEAF_LEVEL, 7);
      perAncestor.set(a, (perAncestor.get(a) ?? 0) + 1);
    }
    for (let i = 0; i < mid.ids.length; i++) expect(perAncestor.get(mid.ids[i])).toBe(mid.count[i]);
    // And a viewport count equals what the leaf level shows inside it.
    const atl: BBoxTuple = [-85, 33, -84, 34.5];
    const inside = engine.query(atl, LEAF_LEVEL, null).ids.length;
    expect(engine.counts(atl).visible).toBe(inside);
  });

  it("applies filters to every count", () => {
    const { engine, cams } = setup();
    const f: EngineFilter = { flock: false, alpr: true, user: false, operator: "police" };
    const b = engine.build(f);
    const expected = countedCams(cams, f);
    expect(b.included).toBe(expected);
    expect(engine.query(WORLD, 8, null).count.reduce((a, c) => a + c, 0)).toBe(expected);
    expect(b.hex[3].count.reduce((a, c) => a + c, 0)).toBe(expected);
    const c = engine.counts(WORLD);
    expect(c.visible).toBe(expected);
    expect(c.byKind.flock).toBe(0);
    expect(c.totalByKind.flock).toBeGreaterThan(0); // "filters hide N" stays available
  });

  it("links each node to its ancestor for split and merge animations", () => {
    const { engine } = setup();
    engine.build(ALL);
    const coarse = engine.query(WORLD, 8, null);
    const fine = engine.query(WORLD, 9, { level: 8, ids: coarse.ids });
    expect(fine.dir).toBe("in");
    const coarseIds = new Set(coarse.ids);
    // Every new node's ancestor is a node that was on screen, and their counts add up.
    const sums = new Map<number, number>();
    fine.anc!.forEach((a, i) => {
      expect(coarseIds.has(a)).toBe(true);
      sums.set(a, (sums.get(a) ?? 0) + fine.count[i]);
    });
    coarse.ids.forEach((id, i) => expect(sums.get(id)).toBe(coarse.count[i]));

    const back = engine.query(WORLD, 8, { level: 9, ids: fine.ids });
    expect(back.dir).toBe("out");
    const backIds = new Set(back.ids);
    for (const a of back.anc!) expect(backIds.has(a)).toBe(true);

    // Multi-level jumps (scroll-wheel spinning) resolve too.
    const leaf = engine.query(WORLD, LEAF_LEVEL, { level: CLUSTER_MIN_LEVEL, ids: engine.query(WORLD, CLUSTER_MIN_LEVEL, null).ids });
    expect(leaf.anc!.length).toBe(leaf.ids.length);
  });

  it("reports unverified submissions separately inside clusters", () => {
    const { engine } = setup();
    engine.build(ALL);
    const q = engine.query(WORLD, 7, null);
    const users = q.users.reduce((a, c) => a + c, 0);
    expect(users).toBe(3);
    engine.build({ ...ALL, user: false });
    expect(engine.query(WORLD, 7, null).users.reduce((a, c) => a + c, 0)).toBe(0);
  });

  it("never averages a cluster across the antimeridian", () => {
    const { engine } = setup();
    engine.build(ALL);
    const fiji: BBoxTuple = [178, -18, -178, -15]; // crosses 180
    for (let L = CLUSTER_MIN_LEVEL; L <= NEAR_MAX_LEVEL; L++) {
      const q = engine.query(fiji, L, null);
      expect(q.ids.length, `level ${L}`).toBeGreaterThan(0);
      for (let i = 0; i < q.ids.length; i++) {
        const lon = (q.x[i] - 0.5) * 360;
        // A centre in the middle of the Pacific/Atlantic would mean an average across the seam.
        expect(Math.abs(lon)).toBeGreaterThan(178);
      }
    }
  });

  it("wraps hex cells at the antimeridian instead of splitting them", () => {
    const g = hexGrid(WIDE_MAX_LEVEL);
    const y = 0.56;
    const east = hexCell(g, lonToMerc(179.99), y);
    const west = hexCell(g, lonToMerc(-179.99), y);
    expect(east).toEqual(west);
    const [cx] = hexCenter(g, east[0], east[1]);
    expect(Math.abs(cx - Math.round(cx))).toBeLessThan(g.R); // centred on the seam
  });

  it("keeps co-located cameras as separate leaves at the leaf level", () => {
    const { engine } = setup();
    engine.build(ALL);
    const q = engine.query([-100.01, 39.99, -99.99, 40.01], LEAF_LEVEL, null);
    expect(q.ids.length).toBe(5);
    expect(new Set(q.leaf).size).toBe(5);
  });

  it("expands a cluster to the bounds of exactly its cameras", () => {
    const { engine } = setup();
    engine.build(ALL);
    const q = engine.query(WORLD, 7, null);
    const i = q.count.indexOf(Math.max(...q.count));
    const ex = engine.expand(q.ids[i])!;
    expect(ex.zoom).toBeGreaterThan(7);
    const [w, s, e, n] = ex.bounds;
    expect(engine.counts([w, s, e, n]).visible).toBeGreaterThanOrEqual(q.count[i]);
    expect(engine.expand(-12345)).toBeNull();
  });

  it("builds the full dataset size within budget", () => {
    const { engine } = setup(150_000, 0);
    const b = engine.build(ALL);
    // Recorded for the report; the worker does this once per data change, off the main thread.
    console.info(`build 150k: index ${b.buildMs.toFixed(0)} ms, hex ${b.hexMs.toFixed(0)} ms`);
    expect(b.buildMs).toBeLessThan(5000);
    const t = performance.now();
    for (let L = CLUSTER_MIN_LEVEL; L <= LEAF_LEVEL; L++) engine.query([-90, 30, -80, 38], L, null);
    console.info(`queries ${CLUSTER_MIN_LEVEL}..${LEAF_LEVEL}: ${(performance.now() - t).toFixed(1)} ms total`);
  });
});
