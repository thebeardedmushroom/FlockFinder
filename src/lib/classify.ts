import { ACCENT_CSS, STATE_CSS } from "./ramp";
import type { Camera, Category } from "./types";

/**
 * Marker categories the UI renders. `unknown` OSM results fall under `alpr`. `wifi` is
 * the separate, heuristic Wi-Fi fingerprint layer (suspected devices, never confirmed).
 */
export type MarkerKind = "flock" | "alpr" | "user" | "wifi";

/**
 * Camera data uses one accent hue; Flock and other ALPR differ by shape (filled vs ring),
 * not hue. Unverified submissions carry the second, state hue. Wi-Fi sightings are a
 * separate heuristic layer with its own colour.
 */
export const MARKER_COLORS: Record<MarkerKind, string> = {
  flock: ACCENT_CSS,
  alpr: ACCENT_CSS,
  user: STATE_CSS,
  wifi: "#38bdf8",
};

/** Kinds drawn as rings rather than filled discs. */
export const MARKER_HOLLOW: Record<MarkerKind, boolean> = { flock: false, alpr: true, user: false, wifi: false };

export const CATEGORY_LABELS: Record<MarkerKind, string> = {
  flock: "Flock (confirmed)",
  alpr: "ALPR (vendor unknown)",
  user: "Unverified (your submissions)",
  wifi: "Wi-Fi sighting (suspected)",
};

const FLOCK_TAGS = ["brand", "manufacturer", "operator"] as const;

/**
 * Classify an OSM element by its tags, mirroring the Rust rule:
 *  - flock   — any of brand / manufacturer / operator matches /flock/i
 *  - alpr    — surveillance:type=ALPR present, no Flock match
 *  - unknown — anything else
 */
export function classify(tags: Record<string, string | undefined>): Category {
  for (const key of FLOCK_TAGS) {
    const v = tags[key];
    if (typeof v === "string" && /flock/i.test(v)) return "flock";
  }
  const type = tags["surveillance:type"];
  if (typeof type === "string" && type.toLowerCase() === "alpr") return "alpr";
  return "unknown";
}

export function markerKind(category: Category): MarkerKind {
  return category === "flock" ? "flock" : "alpr";
}

export function cameraKind(camera: Pick<Camera, "category">): MarkerKind {
  return markerKind(camera.category);
}

export function kindLabel(kind: MarkerKind): string {
  return CATEGORY_LABELS[kind];
}

/** Vendor name for display, best effort. */
export function vendorLabel(tags: Record<string, string>): string | null {
  return tags.brand ?? tags.manufacturer ?? null;
}
