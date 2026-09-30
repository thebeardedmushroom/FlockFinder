import { describe, expect, it } from "vitest";
import { blockerFor, navDistance, stepDetail, type Readiness, type StepView } from "../lib/nav";
import { cumulative, remainingLine } from "../map/navCamera";

const ready: Readiness = {
  platform: "android",
  device_location: true,
  precise: true,
  approximate: true,
  denied_permanently: false,
  location_enabled: true,
  notifications: true,
  play_services: true,
  power_save_gps_off: false,
};

describe("navigation", () => {
  it("shows feet under a tenth of a mile, miles above", () => {
    expect(navDistance(152)).toBe("500 ft");
    expect(navDistance(150)).toBe("500 ft");
    expect(navDistance(170)).toBe("0.1 mi");
    expect(navDistance(1609.344 * 2.34)).toBe("2.3 mi");
  });

  it("says what stops navigation from starting", () => {
    expect(blockerFor(ready, false)).toBeNull();
    expect(blockerFor({ ...ready, precise: false }, false)?.kind).toBe("approximate");
    expect(blockerFor({ ...ready, precise: false, approximate: false }, false)?.kind).toBe("permission");
    expect(blockerFor({ ...ready, precise: false, approximate: false, denied_permanently: true }, false)?.kind).toBe("denied");
    expect(blockerFor({ ...ready, location_enabled: false }, false)?.kind).toBe("location_off");
    // The simulator doesn't need real location to be precise or on.
    expect(blockerFor({ ...ready, precise: false, location_enabled: false }, true)).toBeNull();
    const desktop = { ...ready, platform: "desktop" as const, device_location: false };
    expect(blockerFor(desktop, false)?.kind).toBe("desktop");
    expect(blockerFor(desktop, true)).toBeNull();
  });

  it("names the road under the instruction only when the instruction doesn't", () => {
    const step: StepView = {
      index: 1,
      kind: 10,
      icon: "right",
      instruction: "Turn right onto US 29/US 78/North Avenue Northeast.",
      street: "US 29 / US 78 / North Avenue Northeast",
      distance_m: 100,
      exit_number: null,
      roundabout_exit_count: null,
      bearing_before: null,
      bearing_after: null,
    };
    expect(stepDetail(step)).toBeNull();
    expect(stepDetail({ ...step, instruction: "Turn right." })).toBe("US 29 / US 78 / North Avenue Northeast");
    expect(stepDetail({ ...step, instruction: "Take exit 239.", exit_number: "239", street: "US 19" })).toBe("Exit 239 · US 19");
  });

  it("draws only the part of the route still ahead", () => {
    const shape: [number, number][] = [
      [39.0, -105.0],
      [39.01, -105.0],
      [39.01, -104.99],
    ];
    const cum = cumulative(shape);
    const line = remainingLine(shape, cum, cum[1] / 2);
    expect(line).toHaveLength(3);
    expect(line[0][1]).toBeCloseTo(39.005, 4);
    expect(line[0][0]).toBeCloseTo(-105.0, 6);
    expect(remainingLine(shape, cum, cum[2] + 10)[0]).toEqual([-104.99, 39.01]);
  });
});
