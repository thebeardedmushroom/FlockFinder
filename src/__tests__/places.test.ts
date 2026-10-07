import { describe, expect, it } from "vitest";
import {
  customLimitReached,
  labelError,
  moveItem,
  orderedPlaces,
  placeSlots,
  placeSuggestions,
  savedPlaceAt,
  suggestedLabel,
} from "../lib/places";
import type { PlaceKind, SavedPlace } from "../lib/types";

let nextId = 1;
function place(kind: PlaceKind, label: string, sort_order = 0, lat = 39.7, lon = -104.9): SavedPlace {
  return { id: nextId++, kind, label, address: `${label} address`, lat, lon, created_at: 0, sort_order };
}

describe("saved place order", () => {
  it("puts Home and Work first (even unset), then custom places by sort order", () => {
    const places = [place("custom", "Gym", 1), place("work", "Work"), place("custom", "School", 0)];
    const slots = placeSlots(places);
    expect(slots.map((s) => s.kind)).toEqual(["home", "work", "custom", "custom"]);
    expect(slots[0].place).toBeNull();
    expect(slots.slice(2).map((s) => s.place?.label)).toEqual(["School", "Gym"]);
    expect(orderedPlaces(places).map((p) => p.label)).toEqual(["Work", "School", "Gym"]);
  });
});

describe("Directions suggestions", () => {
  const places = [place("home", "Home"), place("custom", "Gym", 0), place("custom", "Grandma's", 1)];
  it("offers every saved place for an empty field", () => {
    expect(placeSuggestions(places, "  ").map((p) => p.label)).toEqual(["Home", "Gym", "Grandma's"]);
  });
  it("filters by label, ignoring case", () => {
    expect(placeSuggestions(places, "g").map((p) => p.label)).toEqual(["Gym", "Grandma's"]);
    expect(placeSuggestions(places, "GRAND").map((p) => p.label)).toEqual(["Grandma's"]);
    expect(placeSuggestions(places, "office")).toEqual([]);
  });
});

describe("label validation", () => {
  const places = [place("home", "Home"), place("custom", "Gym")];
  it("requires a unique, non-empty name and keeps Home and Work reserved", () => {
    expect(labelError(places, "")).toMatch(/Enter a name/);
    expect(labelError(places, "gym")).toMatch(/already have a place named "gym"/);
    expect(labelError(places, "WORK")).toMatch(/reserved for your Work address/);
    expect(labelError(places, "x".repeat(61))).toMatch(/60 characters/);
    expect(labelError(places, "Office")).toBeNull();
  });
  it("lets a place keep its own name", () => {
    expect(labelError(places, "GYM", places[1].id)).toBeNull();
  });
  it("caps custom places at 10", () => {
    const ten = Array.from({ length: 10 }, (_, i) => place("custom", `P${i}`, i));
    expect(customLimitReached([place("home", "Home"), ...ten.slice(1)])).toBe(false);
    expect(customLimitReached(ten)).toBe(true);
    expect(customLimitReached(ten, ten[0].id)).toBe(false);
  });
});

describe("matching a map point to a saved place", () => {
  it("finds the nearest place within 30 m", () => {
    const a = place("custom", "A", 0, 39.75, -104.99);
    const b = place("custom", "B", 1, 39.7501, -104.99); // ~11 m north of A
    expect(savedPlaceAt([a, b], 39.75008, -104.99)?.label).toBe("B");
    expect(savedPlaceAt([a, b], 39.7495, -104.99)).toBeNull(); // ~55 m from A
  });
});

describe("helpers", () => {
  it("suggests a label from the name or the first line of the address", () => {
    expect(suggestedLabel("Union Station", "1701 Wynkoop St, Denver")).toBe("Union Station");
    expect(suggestedLabel(null, "1600, Larimer Street, Denver")).toBe("1600 Larimer Street");
    expect(suggestedLabel("", "Coors Field, Blake Street, Denver")).toBe("Coors Field");
  });
  it("moves an item", () => {
    expect(moveItem(["a", "b", "c", "d"], 0, 2)).toEqual(["b", "c", "a", "d"]);
    expect(moveItem(["a", "b", "c", "d"], 3, 0)).toEqual(["d", "a", "b", "c"]);
  });
});
