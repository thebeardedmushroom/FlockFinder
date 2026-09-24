import { describe, expect, it } from "vitest";
import { classify, markerKind, vendorLabel } from "../lib/classify";

describe("classify", () => {
  it("returns flock when brand matches /flock/i", () => {
    expect(classify({ brand: "Flock Safety" })).toBe("flock");
    expect(classify({ brand: "FLOCK" })).toBe("flock");
    expect(classify({ brand: "flock group inc" })).toBe("flock");
  });

  it("returns flock when manufacturer or operator matches, even with ALPR tag present", () => {
    expect(classify({ "surveillance:type": "ALPR", manufacturer: "Flock Safety" })).toBe("flock");
    expect(classify({ "surveillance:type": "ALPR", operator: "Flock Safety" })).toBe("flock");
  });

  it("returns alpr when surveillance:type=ALPR and no flock match", () => {
    expect(classify({ "surveillance:type": "ALPR" })).toBe("alpr");
    expect(classify({ "surveillance:type": "alpr", manufacturer: "Axon Enterprise" })).toBe("alpr");
    expect(classify({ "surveillance:type": "ALPR", brand: "Motorola Solutions" })).toBe("alpr");
  });

  it("returns unknown otherwise", () => {
    expect(classify({})).toBe("unknown");
    expect(classify({ man_made: "surveillance" })).toBe("unknown");
    expect(classify({ "surveillance:type": "camera" })).toBe("unknown");
    // "flock" in an unrelated tag does not count
    expect(classify({ name: "Flock of seagulls", "surveillance:type": "ALPR" })).toBe("alpr");
    expect(classify({ description: "flock" })).toBe("unknown");
  });

  it("tolerates undefined tag values", () => {
    expect(classify({ brand: undefined, "surveillance:type": "ALPR" })).toBe("alpr");
  });

  it("maps categories to marker kinds with unknown folded into alpr", () => {
    expect(markerKind("flock")).toBe("flock");
    expect(markerKind("alpr")).toBe("alpr");
    expect(markerKind("unknown")).toBe("alpr");
  });

  it("prefers brand over manufacturer for the vendor label", () => {
    expect(vendorLabel({ brand: "A", manufacturer: "B" })).toBe("A");
    expect(vendorLabel({ manufacturer: "B" })).toBe("B");
    expect(vendorLabel({})).toBeNull();
  });
});
