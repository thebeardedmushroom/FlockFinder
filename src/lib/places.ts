// Saved places: ordering, matching and validation shared by the quick-nav bar, the Saved
// Places screen, the save sheet and the Directions suggestions. Storage is in Rust
// (src-tauri/src/places.rs); these mirror its rules so mistakes show before saving.
import { haversineM } from "./geo";
import type { PlaceKind, SavedPlace } from "./types";

/** Custom places allowed besides Home and Work (`MAX_CUSTOM` in places.rs). */
export const MAX_CUSTOM_PLACES = 10;
/** `MAX_LABEL_CHARS` in places.rs. */
export const MAX_LABEL_CHARS = 60;
/** Closer than this to the destination, a quick trip says "You're already here" instead. */
export const ALREADY_HERE_M = 50;
/** A map point this close to a saved place is that place (filled star, "Edit saved place"). */
export const SAME_PLACE_M = 30;
/** How long a quick trip waits for a location fix. */
export const LOCATION_TIMEOUT_MS = 15_000;

export const SLOT_LABEL: Record<"home" | "work", string> = { home: "Home", work: "Work" };

/** What a quick-nav chip or a Saved Places row stands for: a Home/Work slot (set or not), or a custom place. */
export type PlaceSlot = { kind: "home" | "work"; place: SavedPlace | null } | { kind: "custom"; place: SavedPlace };

export function customPlaces(places: SavedPlace[]): SavedPlace[] {
  return places.filter((p) => p.kind === "custom").sort((a, b) => a.sort_order - b.sort_order || a.id - b.id);
}

export function slotPlace(places: SavedPlace[], kind: "home" | "work"): SavedPlace | null {
  return places.find((p) => p.kind === kind) ?? null;
}

/** Home, Work (always, set or not), then custom places by their saved order. */
export function placeSlots(places: SavedPlace[]): PlaceSlot[] {
  return [
    { kind: "home", place: slotPlace(places, "home") },
    { kind: "work", place: slotPlace(places, "work") },
    ...customPlaces(places).map((place) => ({ kind: "custom" as const, place })),
  ];
}

/** Every saved place in display order (unset slots left out). */
export function orderedPlaces(places: SavedPlace[]): SavedPlace[] {
  return placeSlots(places).flatMap((s) => (s.place ? [s.place] : []));
}

/** Directions suggestions: every place for an empty field, else those whose label contains the text. */
export function placeSuggestions(places: SavedPlace[], text: string): SavedPlace[] {
  const q = text.trim().toLowerCase();
  const all = orderedPlaces(places);
  if (!q) return all;
  return all.filter((p) => p.label.toLowerCase().includes(q));
}

/** Why this label can't be used for a custom place, or null when it can. */
export function labelError(places: SavedPlace[], label: string, exceptId: number | null = null): string | null {
  const l = label.trim();
  if (!l) return "Enter a name for the place.";
  if ([...l].length > MAX_LABEL_CHARS) return `Keep the name to ${MAX_LABEL_CHARS} characters or fewer.`;
  const lower = l.toLowerCase();
  if (lower === "home" || lower === "work") return `"${SLOT_LABEL[lower]}" is reserved for your ${SLOT_LABEL[lower]} address. Choose another name.`;
  if (places.some((p) => p.id !== exceptId && p.label.toLowerCase() === lower)) return `You already have a place named "${l}".`;
  return null;
}

export function customLimitReached(places: SavedPlace[], exceptId: number | null = null): boolean {
  return places.filter((p) => p.kind === "custom" && p.id !== exceptId).length >= MAX_CUSTOM_PLACES;
}

/** The saved place at this point (the nearest within SAME_PLACE_M), if any. */
export function savedPlaceAt(places: SavedPlace[], lat: number, lon: number): SavedPlace | null {
  let best: SavedPlace | null = null;
  let bestM = SAME_PLACE_M;
  for (const p of places) {
    const d = haversineM(lat, lon, p.lat, p.lon);
    if (d <= bestM) {
      best = p;
      bestM = d;
    }
  }
  return best;
}

/** A label to start from: the place's name, else the first line of its address. */
export function suggestedLabel(name: string | null | undefined, address: string): string {
  // Nominatim puts the house number in its own part: "1600, Larimer Street, …" → "1600 Larimer Street".
  const parts = address.split(",").map((x) => x.trim());
  const firstLine = /^\d+[a-z]?$/i.test(parts[0] ?? "") && parts[1] ? `${parts[0]} ${parts[1]}` : parts[0] ?? "";
  const source = (name?.trim() || firstLine).trim();
  return [...source].slice(0, MAX_LABEL_CHARS).join("");
}

export function placeKindLabel(kind: PlaceKind): string {
  return kind === "custom" ? "Saved place" : SLOT_LABEL[kind];
}

/** `items` with the one at `from` moved to `to`. */
export function moveItem<T>(items: T[], from: number, to: number): T[] {
  const out = items.slice();
  const [it] = out.splice(from, 1);
  out.splice(Math.max(0, Math.min(out.length, to)), 0, it);
  return out;
}
