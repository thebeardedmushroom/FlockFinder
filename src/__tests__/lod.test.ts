import { describe, expect, it } from "vitest";
import {
  bandForZoom,
  bandHasLegend,
  densityT,
  formatCount,
  levelScale,
  minProportionalCount,
  NODE_MAX_RADIUS_PX,
  NODE_MIN_RADIUS_PX,
  nodeRadius,
} from "../lib/lod";
import { oklchToRgb, rampLightness, rampRgb, relativeLuminance, STATE_RGB } from "../lib/ramp";
import { anyOverlap, placeLabels, type LabelRequest } from "../lib/labels";

describe("bands", () => {
  it("maps every zoom to exactly one band with the specified breakpoints", () => {
    expect([0, 3.5, 5.99].map(bandForZoom)).toEqual(["wide", "wide", "wide"]);
    expect([6, 8, 10.99].map(bandForZoom)).toEqual(["mid", "mid", "mid"]);
    expect([11, 13.5].map(bandForZoom)).toEqual(["near", "near"]);
    expect([14, 19].map(bandForZoom)).toEqual(["detail", "detail"]);
  });

  it("requires a legend exactly where aggregates are shown", () => {
    expect(bandHasLegend("wide") && bandHasLegend("mid")).toBe(true);
  });
});

describe("encoding honesty", () => {
  it("scales node area, not radius, with count", () => {
    const s = levelScale(10_000);
    expect(nodeRadius(10_000, s)).toBeCloseTo(NODE_MAX_RADIUS_PX);
    // 4× the cameras → 4× the area → 2× the radius (not 4×).
    const a = nodeRadius(1000, s);
    const b = nodeRadius(4000, s);
    expect(b / a).toBeCloseTo(2);
    expect((b * b) / (a * a)).toBeCloseTo(4);
  });

  it("clamps only below the stated minimum", () => {
    const s = levelScale(10_000);
    const c = minProportionalCount(s);
    expect(nodeRadius(c, s)).toBeGreaterThanOrEqual(NODE_MIN_RADIUS_PX);
    expect(nodeRadius(c - 1, s)).toBe(NODE_MIN_RADIUS_PX);
    expect(nodeRadius(2, s)).toBe(NODE_MIN_RADIUS_PX);
  });

  it("puts density on a log scale from 0 to 1", () => {
    expect(densityT(1, 1000)).toBe(0);
    expect(densityT(1000, 1000)).toBe(1);
    expect(densityT(31.6, 1000)).toBeCloseTo(0.5, 2);
  });

  it("formats counts exactly below 1,000 and abbreviates above", () => {
    expect(formatCount(1)).toBe("1");
    expect(formatCount(999)).toBe("999");
    expect(formatCount(1000)).toBe("1k");
    expect(formatCount(1234)).toBe("1.2k");
    expect(formatCount(12_345)).toBe("12k");
    expect(formatCount(150_947)).toBe("151k");
    expect(formatCount(1_250_000)).toBe("1.3M");
  });
});

describe("luminance ramp", () => {
  it("is perceptually uniform and monotonic in lightness", () => {
    let prevL = -1;
    let prevY = -1;
    for (let i = 0; i <= 20; i++) {
      const t = i / 20;
      const L = rampLightness(t);
      expect(L).toBeGreaterThan(prevL);
      if (i > 0) expect(L - prevL).toBeCloseTo(rampLightness(0.05) - rampLightness(0), 9); // equal steps
      const Y = relativeLuminance(rampRgb(t));
      expect(Y).toBeGreaterThan(prevY);
      prevL = L;
      prevY = Y;
    }
  });

  it("keeps one hue: every stop stays in gamut near the accent hue", () => {
    for (let i = 0; i <= 10; i++) {
      const [r, g, b] = rampRgb(i / 10);
      expect(Math.max(r, g, b)).toBeLessThanOrEqual(1);
      // A cyan-family colour: blue and green dominate red at every stop.
      expect(g).toBeGreaterThan(r);
      expect(b).toBeGreaterThan(r);
    }
  });

  it("uses a clearly different hue for state", () => {
    const [r, , b] = STATE_RGB;
    expect(r).toBeGreaterThan(0.5);
    expect(b).toBeGreaterThan(0.8);
    expect(oklchToRgb(0.5, 0.5, 200)).toBeNull(); // out of gamut is reported, not clipped silently
  });
});

describe("label placement", () => {
  const rand = (() => {
    let s = 7;
    return () => ((s = (s * 16807) % 2147483647) / 2147483647);
  })();

  it("never overlaps labels, even at extreme density", () => {
    const reqs: LabelRequest[] = [];
    for (let i = 0; i < 600; i++) {
      const r = 7 + rand() * 20;
      reqs.push({ id: i, x: rand() * 800, y: rand() * 500, r, w: 26, h: 15, priority: Math.round(rand() * 5000) });
    }
    const placed = placeLabels(reqs, 800, 500, reqs);
    expect(placed.length).toBeGreaterThan(20);
    expect(anyOverlap(placed)).toBe(false);
    for (const p of placed) {
      expect(p.x).toBeGreaterThanOrEqual(0);
      expect(p.y).toBeGreaterThanOrEqual(0);
      expect(p.x + p.w).toBeLessThanOrEqual(800);
      expect(p.y + p.h).toBeLessThanOrEqual(500);
    }
  });

  it("places the most important label first and inside its node when it fits", () => {
    const placed = placeLabels(
      [
        { id: 1, x: 100, y: 100, r: 30, w: 30, h: 14, priority: 9000 },
        { id: 2, x: 110, y: 100, r: 8, w: 30, h: 14, priority: 10 },
      ],
      400,
      400,
    );
    expect(placed[0]).toMatchObject({ id: 1, inside: true });
    expect(anyOverlap(placed)).toBe(false);
  });
});
