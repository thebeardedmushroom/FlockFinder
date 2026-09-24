import { describe, expect, it } from "vitest";
import { DEFAULT_FOV, MAX_CONES, formatDirection, parseDirectionValue, parseDirections } from "../lib/direction";

describe("parseDirectionValue", () => {
  it("parses plain degrees with the default field of view", () => {
    expect(parseDirectionValue("75")).toEqual([{ bearing: 75, width: DEFAULT_FOV }]);
    expect(parseDirectionValue(" 12.5 ")).toEqual([{ bearing: 12.5, width: DEFAULT_FOV }]);
  });

  it("normalises out-of-range degrees", () => {
    expect(parseDirectionValue("-90")[0].bearing).toBe(270);
    expect(parseDirectionValue("450")[0].bearing).toBe(90);
    expect(parseDirectionValue("360")[0].bearing).toBe(0);
  });

  it("parses cardinal letters case-insensitively", () => {
    expect(parseDirectionValue("NE")[0].bearing).toBe(45);
    expect(parseDirectionValue("nnw")[0].bearing).toBe(337.5);
  });

  it("gives one cone per ;-separated value and drops duplicates", () => {
    expect(parseDirectionValue("71;132;189;0;0").map((c) => c.bearing)).toEqual([71, 132, 189, 0]);
    expect(parseDirectionValue("90; 270")).toHaveLength(2);
  });

  it("treats a-b as a clockwise range, including across north", () => {
    expect(parseDirectionValue("10-55")).toEqual([{ bearing: 32.5, width: 45 }]);
    expect(parseDirectionValue("338-23")).toEqual([{ bearing: 0.5, width: 45 }]);
    expect(parseDirectionValue("N-E")).toEqual([{ bearing: 45, width: 90 }]);
    expect(parseDirectionValue("90-90")).toEqual([{ bearing: 90, width: DEFAULT_FOV }]);
  });

  it("skips unparseable parts and caps the number of cones", () => {
    expect(parseDirectionValue("forward")).toEqual([]);
    expect(parseDirectionValue("abc;90;")).toEqual([{ bearing: 90, width: DEFAULT_FOV }]);
    expect(parseDirectionValue("0;40;80;120;160;200;240;280")).toHaveLength(MAX_CONES);
  });
});

describe("parseDirections", () => {
  it("prefers direction and falls back to camera:direction", () => {
    expect(parseDirections({ direction: "10", "camera:direction": "200" })[0].bearing).toBe(10);
    expect(parseDirections({ "camera:direction": "200" })[0].bearing).toBe(200);
    expect(parseDirections({})).toEqual([]);
  });
});

describe("formatDirection", () => {
  it("labels plain degrees and keeps the raw value for lists and ranges", () => {
    expect(formatDirection("90")).toBe("E (90°)");
    expect(formatDirection("90;270")).toBe("E (90°), W (270°) · 90;270");
    expect(formatDirection("338-23")).toBe("N (1°) · 338-23");
    expect(formatDirection("garbage")).toBe("garbage");
  });
});
