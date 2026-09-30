/**
 * Map themes. Each theme is a bundled OpenFreeMap base style (the same styles the provider
 * serves at tiles.openfreemap.org/styles/…, snapshotted so a theme applies instantly and
 * offline) plus the transforms in `basemap.ts`, and an overlay palette that keeps camera data,
 * selections and drawn geometry readable on that ground.
 *
 * Adding a theme: add an id to `THEME_IDS`, an entry to `THEMES` (a base style, a palette for
 * the roles you want to change, and `DARK_OVERLAY`/`LIGHT_OVERLAY` or a variant), then run
 * `npm test`: the theme tests check it builds and that every overlay passes the contrast check.
 */
import type * as maplibregl from "maplibre-gl";
import { ACCENT_HUE, ACCENT_RGB, makeRamp, oklchInGamut, rampRgb, relativeLuminance, rgbCss, STATE_HUE, STATE_RGB, type Ramp, type Rgb } from "../lib/ramp";
import { latinLabelsAtWideZoom, parseCssColor, recolorLayers, stripCamera, tameLayers, type BasemapPalette } from "./basemap";
import darkBase from "./styles/openfreemap-dark.json";
import fiordBase from "./styles/openfreemap-fiord.json";
import positronBase from "./styles/openfreemap-positron.json";

export type Scheme = "dark" | "light";

// ---------------------------------------------------------------------------
// Overlay palettes: everything this app draws over the basemap
// ---------------------------------------------------------------------------

export interface OverlayPalette {
  scheme: Scheme;
  /** Individual cameras: Flock as a filled disc, other ALPR as a ring. */
  accent: Rgb;
  /** Rim of a filled camera disc. */
  accentEdge: Rgb;
  /** Inside of an ALPR ring, and its opacity. */
  ringFill: Rgb;
  ringFillAlpha: number;
  /** Unverified submissions (fill and rim). */
  state: Rgb;
  stateEdge: Rgb;
  /** Cluster nodes and the density field: sparse fades toward the ground, dense stands out. */
  ramp: Ramp;
  /** Ramp position of the smallest cluster's outline (it runs up the ramp with size). */
  clusterEdge: number;
  /** Additive glow around nodes. It can only brighten, so a light ground turns it off. */
  halos: boolean;
  /** Stroke of stale cameras (missing from the latest sync). */
  stale: string;
  wifi: string;
  wifiStroke: string;
  /** Rim of Wi-Fi sightings imported from your own scans. */
  wifiImportedStroke: string;
  /** Marks on or around a coloured marker: Wi-Fi cluster outline and count, drawn-route points. */
  markerInk: string;
  /** Selection ring (`highlight`), the rings of other highlighted cameras, and the glow behind both. */
  highlight: string;
  highlightDim: string;
  highlightGlow: string;
  highlightGlowDim: string;
  /** Watch areas, saved routes, the route being drawn. */
  area: string;
  route: string;
  draw: string;
  /** Directions: the camera-avoiding route, the fastest route (muted), the dark or light
   *  casing under both (so they read over any road colour), and the ring on cameras still on
   *  the chosen route. */
  routeAvoid: string;
  routeFast: string;
  routeCasing: string;
  routeCamera: string;
  /** Veil the load sweep draws over the map until the data arrives. */
  sweepShade: string;
}

const INK: Rgb = [0.02, 0.035, 0.05];

/** The palette the app was designed on: bright marks on a near-black ground. */
export const DARK_OVERLAY: OverlayPalette = {
  scheme: "dark",
  accent: ACCENT_RGB,
  accentEdge: rampRgb(0.98),
  ringFill: INK,
  ringFillAlpha: 0.75,
  state: STATE_RGB,
  stateEdge: [0.96, 0.96, 0.96],
  ramp: rampRgb,
  clusterEdge: 0.4,
  halos: true,
  stale: rgbCss(rampRgb(0.55), 0.7),
  wifi: "#38bdf8",
  wifiStroke: "#05080f",
  wifiImportedStroke: "#f8fafc",
  markerInk: "#05080f",
  highlight: "#ffffff",
  highlightDim: "rgba(255, 255, 255, 0.7)",
  highlightGlow: "rgba(255, 255, 255, 0.14)",
  highlightGlowDim: "rgba(255, 255, 255, 0.1)",
  area: "#00e5ff",
  route: "#3dffa7",
  draw: "#a78bfa",
  routeAvoid: "#3dffa7",
  routeFast: "#c9d3de",
  routeCasing: "#05080f",
  routeCamera: "#ff8a3d",
  sweepShade: "rgba(5, 8, 15, 0.62)",
};

const LIGHT_ACCENT = oklchInGamut(0.5, 0.11, ACCENT_HUE);
const LIGHT_INK: Rgb = [0.05, 0.08, 0.12];

/** Dark marks on a pale ground: the same hues, lightness flipped. */
export const LIGHT_OVERLAY: OverlayPalette = {
  scheme: "light",
  accent: LIGHT_ACCENT,
  accentEdge: LIGHT_INK,
  ringFill: [0.98, 0.99, 1],
  ringFillAlpha: 0.85,
  state: oklchInGamut(0.5, 0.2, STATE_HUE),
  stateEdge: LIGHT_INK,
  ramp: makeRamp(0.84, 0.3),
  // Pale fills fade into the ground by design, so their outline carries the contrast.
  clusterEdge: 0.7,
  halos: false,
  stale: rgbCss(LIGHT_ACCENT, 0.75),
  wifi: "#0369a1",
  wifiStroke: "#ffffff",
  wifiImportedStroke: "#0b1220",
  markerInk: "#ffffff",
  highlight: "#0b1220",
  highlightDim: "rgba(11, 18, 32, 0.75)",
  highlightGlow: "rgba(11, 18, 32, 0.16)",
  highlightGlowDim: "rgba(11, 18, 32, 0.1)",
  area: "#0e7490",
  route: "#047857",
  draw: "#6d28d9",
  routeAvoid: "#047857",
  routeFast: "#5b6b80",
  routeCasing: "#ffffff",
  routeCamera: "#c2410c",
  sweepShade: "rgba(236, 238, 234, 0.62)",
};

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

export const THEME_IDS = ["midnight", "navy", "night-vision", "paper", "muted"] as const;
export type ThemeId = (typeof THEME_IDS)[number];

export interface MapTheme {
  id: ThemeId;
  name: string;
  scheme: Scheme;
  description: string;
  /** Where the look comes from: the provider's own style, or a palette authored here. */
  origin: "openfreemap" | "authored";
  base: maplibregl.StyleSpecification;
  /** Mute pass (see `tameLayers`), applied before the palette. */
  tame?: boolean;
  palette?: Partial<BasemapPalette>;
  overlay: OverlayPalette;
}

const NAVY_RAMP = makeRamp(0.52, 0.97);

const DARK = darkBase as unknown as maplibregl.StyleSpecification;
const FIORD = fiordBase as unknown as maplibregl.StyleSpecification;
const POSITRON = positronBase as unknown as maplibregl.StyleSpecification;

export const THEMES: Record<ThemeId, MapTheme> = {
  midnight: {
    id: "midnight",
    name: "Midnight",
    scheme: "dark",
    description: "Near-black, muted grey roads, low-contrast labels",
    origin: "openfreemap",
    base: DARK,
    tame: true,
    overlay: DARK_OVERLAY,
  },
  navy: {
    id: "navy",
    name: "Navy",
    scheme: "dark",
    description: "Dark blue-grey land, deep blue water, soft white major roads",
    origin: "openfreemap",
    base: FIORD,
    // Fiord draws major roads darker than the land; lift them to a soft white. Its
    // residential fill is near-white, which no overlay colour could stand out from.
    palette: {
      landuse: "#3d4862",
      water: "#232c40",
      motorway: "hsla(220, 30%, 90%, 0.8)",
      primary: "hsla(220, 25%, 84%, 0.55)",
      secondary: "hsla(222, 20%, 66%, 0.4)",
      casing: "hsl(224, 22%, 36%)",
    },
    // The ground is mid-dark, so the ramp starts lighter to keep small clusters off it.
    overlay: { ...DARK_OVERLAY, ramp: NAVY_RAMP, clusterEdge: 0.5, stale: rgbCss(NAVY_RAMP(0.6), 0.75), draw: "#c4b5fd" },
  },
  "night-vision": {
    id: "night-vision",
    name: "Night Vision",
    scheme: "dark",
    description: "Very dark and desaturated, amber and red road accents",
    origin: "authored",
    base: DARK,
    palette: {
      background: "#0a0807",
      landuse: "#110d0b",
      green: "#100f0a",
      ice: "#141110",
      water: "#1d1714",
      waterway: "#231b17",
      building: "#171210",
      buildingOutline: "#221a16",
      aeroway: "#1a1411",
      // Only the top tiers carry colour; the rest of the network stays near the ground.
      motorway: "#b8480f",
      primary: "#86310f",
      secondary: "#3d2217",
      minor: "#261914",
      path: "#211612",
      rail: "#3a2a22",
      casing: "#1c0f0a",
      boundary: "#6b4a37",
      label: "#a8835f",
      roadLabel: "#8f6545",
      waterLabel: "#6f5d53",
      labelHalo: "#0a0807",
    },
    overlay: DARK_OVERLAY,
  },
  paper: {
    id: "paper",
    name: "Paper",
    scheme: "light",
    description: "Off-white land, light grey roads, standard labels",
    origin: "openfreemap",
    base: POSITRON,
    overlay: LIGHT_OVERLAY,
  },
  muted: {
    id: "muted",
    name: "Muted",
    scheme: "light",
    description: "Pale neutral palette, reduced saturation, subtle water",
    origin: "authored",
    base: POSITRON,
    // Flat stone tones: roads a shade darker than the land and barely outlined, so the map
    // reads as texture and the camera data carries all the contrast.
    palette: {
      background: "#e3e0da",
      landuse: "#dcd8d1",
      green: "#d7dacd",
      ice: "#eceae6",
      water: "#cad2d5",
      waterway: "#c2cbcf",
      building: "#d3cec6",
      buildingOutline: "#c9c3ba",
      aeroway: "#d8d4cd",
      motorway: "#bdb6ab",
      primary: "#c7c1b7",
      secondary: "#cfcac1",
      minor: "#d6d2ca",
      path: "#d4cfc7",
      rail: "#b6b0a6",
      casing: "#d9d5ce",
      boundary: "#a19b91",
      label: "#58534c",
      roadLabel: "#77716a",
      waterLabel: "#5f6c74",
      labelHalo: "#e6e3dd",
    },
    overlay: LIGHT_OVERLAY,
  },
};

export function isThemeId(v: unknown): v is ThemeId {
  return typeof v === "string" && (THEME_IDS as readonly string[]).includes(v);
}

/** The finished style for a theme. The bundled base is never modified. */
export function buildThemeStyle(theme: MapTheme): maplibregl.StyleSpecification {
  const style = structuredClone(theme.base);
  stripCamera(style);
  if (theme.tame) tameLayers(style.layers);
  if (theme.palette) recolorLayers(style.layers, theme.palette);
  latinLabelsAtWideZoom(style.layers);
  style.name = `Flock Finder ${theme.name}`;
  return style;
}

/** Background colour of a built style (for the map container before tiles arrive). */
export function styleBackground(style: maplibregl.StyleSpecification): string | null {
  const bg = style.layers.find((l) => l.type === "background") as maplibregl.BackgroundLayerSpecification | undefined;
  const c = bg?.paint?.["background-color"];
  return typeof c === "string" ? c : null;
}

/**
 * Overlay palette for a style of unknown design (a custom URL): whichever suits its background.
 * Even after the mute pass a light style stays mid-grey, where the dark palette's white marks fade.
 */
export function overlayForStyle(style: maplibregl.StyleSpecification): OverlayPalette {
  const bg = styleBackground(style);
  const c = bg ? parseCssColor(bg) : null;
  return c && relativeLuminance([c[0], c[1], c[2]]) > 0.3 ? LIGHT_OVERLAY : DARK_OVERLAY;
}

// ---------------------------------------------------------------------------
// The user's choice
// ---------------------------------------------------------------------------

/** A theme, "system" (Paper on a light OS, Midnight on a dark one) or "custom" (the style URL setting). */
export type ThemeChoice = ThemeId | "system" | "custom";

export const DEFAULT_LIGHT: ThemeId = "paper";
export const DEFAULT_DARK: ThemeId = "midnight";
/** The style URL setting's default; anything else there predates themes and was chosen on purpose. */
export const DEFAULT_STYLE_URL = "https://tiles.openfreemap.org/styles/dark";

const STORAGE_KEY = "flockfinder.mapTheme";

export function isThemeChoice(v: unknown): v is ThemeChoice {
  return v === "system" || v === "custom" || isThemeId(v);
}

/** The saved choice, or null when none is saved or it names a theme that no longer exists. */
export function loadThemeChoice(): ThemeChoice | null {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return isThemeChoice(v) ? v : null;
  } catch {
    return null;
  }
}

export function saveThemeChoice(choice: ThemeChoice): void {
  try {
    localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    /* storage unavailable: the choice lasts for this session only */
  }
}

/**
 * Nothing saved yet: follow the OS, unless the style URL setting was changed from its default
 * before themes existed (then keep showing that style).
 */
export function effectiveChoice(saved: ThemeChoice | null, styleUrl: string): ThemeChoice {
  if (saved) return saved;
  return styleUrl.trim() !== DEFAULT_STYLE_URL ? "custom" : "system";
}

export type ResolvedMapStyle =
  | { kind: "theme"; key: string; theme: MapTheme; overlay: OverlayPalette; scheme: Scheme }
  | { kind: "custom"; key: string; url: string; overlay: OverlayPalette; scheme: Scheme };

export function resolveMapStyle(choice: ThemeChoice, styleUrl: string, systemDark: boolean): ResolvedMapStyle {
  if (choice === "custom") {
    const url = styleUrl.trim();
    // Custom styles get the mute pass, which lands them on a dark ground.
    return { kind: "custom", key: `custom:${url}`, url, overlay: DARK_OVERLAY, scheme: "dark" };
  }
  const theme = THEMES[choice === "system" ? (systemDark ? DEFAULT_DARK : DEFAULT_LIGHT) : choice];
  return { kind: "theme", key: theme.id, theme, overlay: theme.overlay, scheme: theme.scheme };
}

