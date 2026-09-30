import { describe, expect, it } from "vitest";
import { roadWeight } from "../map/basemap";

describe("roadWeight", () => {
  it("ranks OpenMapTiles road layers by importance", () => {
    expect(roadWeight("highway_motorway_inner", "transportation")).toBe(1);
    expect(roadWeight("highway_major_casing", "transportation")).toBe(0.8);
    expect(roadWeight("road_secondary_tertiary", "transportation")).toBe(0.65);
    expect(roadWeight("highway_minor", "transportation")).toBe(0.5);
    expect(roadWeight("highway_path", "transportation")).toBe(0.35);
    expect(roadWeight("railway", "transportation")).toBe(0.35);
  });

  it("leaves non-road layers and rail dash overlays to the mute pass", () => {
    expect(roadWeight("railway_dashline", "transportation")).toBeNull();
    expect(roadWeight("waterway", "waterway")).toBeNull();
    expect(roadWeight("boundary_state", "boundary")).toBeNull();
    expect(roadWeight("highway_name", undefined)).toBeNull();
  });
});
