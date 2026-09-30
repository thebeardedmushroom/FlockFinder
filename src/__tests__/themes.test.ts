import type * as maplibregl from "maplibre-gl";
import { beforeEach, describe, expect, it } from "vitest";
import { contrastRatio, type Rgb } from "../lib/ramp";
import { layerRole, mapOutputs, parseCssColor, type BasemapPalette } from "../map/basemap";
import {
  buildThemeStyle,
  DEFAULT_STYLE_URL,
  effectiveChoice,
  loadThemeChoice,
  overlayForStyle,
  resolveMapStyle,
  saveThemeChoice,
  THEME_IDS,
  THEMES,
  type OverlayPalette,
} from "../map/themes";

type Paint = Record<string, unknown>;
const paintOf = (l: maplibregl.LayerSpecification) => ((l as { paint?: Paint }).paint ?? {}) as Paint;

/** Every output a paint value can take (zoom curves unrolled, data expressions searched). */
function literals(v: unknown): unknown[] {
  const out: unknown[] = [];
  mapOutputs(v, (x) => {
    const walk = (y: unknown) => {
      if (Array.isArray(y)) y.forEach(walk);
      else out.push(y);
    };
    walk(x);
    return x;
  });
  return out;
}

function colorsOf(v: unknown): [number, number, number, number][] {
  return literals(v)
    .filter((x): x is string => typeof x === "string")
    .map(parseCssColor)
    .filter((c): c is [number, number, number, number] => c !== null);
}

function over(top: [number, number, number, number], alpha: number, bottom: Rgb): Rgb {
  const a = top[3] * alpha;
  return [0, 1, 2].map((i) => top[i] * a + bottom[i] * (1 - a)) as Rgb;
}

/** The colours camera markers can sit on: land, built-up land, parks, water. */
function grounds(style: maplibregl.StyleSpecification): { name: string; rgb: Rgb }[] {
  const bgLayer = style.layers.find((l) => l.type === "background")!;
  const bg = colorsOf(paintOf(bgLayer)["background-color"])[0];
  const base: Rgb = [bg[0], bg[1], bg[2]];
  const out = [{ name: "background", rgb: base }];
  for (const layer of style.layers) {
    const role = layerRole(layer);
    if (layer.type !== "fill" || !role || !["water", "green", "landuse"].includes(role)) continue;
    const p = paintOf(layer);
    const opacities = literals(p["fill-opacity"] ?? 1).filter((x): x is number => typeof x === "number" && x <= 1);
    const alpha = Math.max(...opacities, 0);
    for (const c of colorsOf(p["fill-color"])) out.push({ name: `${layer.id}`, rgb: over(c, alpha, base) });
  }
  return out;
}

const rgb = (css: string): Rgb => {
  const c = parseCssColor(css)!;
  return [c[0], c[1], c[2]];
};

/** Each mark is visible when its fill or its outline stands out from the ground. */
function marks(o: OverlayPalette): { name: string; colors: Rgb[] }[] {
  return [
    { name: "Flock camera", colors: [o.accent, o.accentEdge] },
    { name: "ALPR camera ring", colors: [o.accent] },
    { name: "unverified submission", colors: [o.state, o.stateEdge] },
    { name: "Wi-Fi sighting", colors: [rgb(o.wifi), rgb(o.wifiStroke)] },
    { name: "smallest cluster", colors: [o.ramp(0.18), o.ramp(o.clusterEdge)] },
    { name: "largest cluster", colors: [o.ramp(1), o.ramp(1)] },
    { name: "stale camera", colors: [rgb(o.stale)] },
    { name: "selection ring", colors: [rgb(o.highlight)] },
    { name: "watch area", colors: [rgb(o.area)] },
    { name: "route", colors: [rgb(o.route)] },
    { name: "route being drawn", colors: [rgb(o.draw)] },
    { name: "avoidance route", colors: [rgb(o.routeAvoid), rgb(o.routeCasing)] },
    { name: "fastest route", colors: [rgb(o.routeFast), rgb(o.routeCasing)] },
    { name: "camera still on route", colors: [rgb(o.routeCamera), rgb(o.routeCasing)] },
    { name: "directions start/end", colors: [rgb(o.highlight), rgb(o.routeCasing)] },
  ];
}

/** WCAG 2.1 minimum for graphical objects (1.4.11 non-text contrast). */
const MIN_CONTRAST = 3;

describe("map themes", () => {
  it("ships three dark and two light themes", () => {
    const schemes = THEME_IDS.map((id) => THEMES[id].scheme);
    expect(schemes.filter((s) => s === "dark")).toHaveLength(3);
    expect(schemes.filter((s) => s === "light")).toHaveLength(2);
  });

  it("builds every theme without touching the bundled base style", () => {
    for (const id of THEME_IDS) {
      const theme = THEMES[id];
      const before = JSON.stringify(theme.base);
      const style = buildThemeStyle(theme);
      expect(JSON.stringify(theme.base)).toBe(before);
      expect(style.version).toBe(8);
      expect(style.center).toBeUndefined();
      expect(style.zoom).toBeUndefined();
      expect(style.sources.openmaptiles).toBeDefined();
      // Building twice gives the same style (no state leaks between builds).
      expect(JSON.stringify(buildThemeStyle(theme))).toBe(JSON.stringify(style));
    }
  });

  it("gives every theme its own background and water", () => {
    const seen = new Set<string>();
    for (const id of THEME_IDS) {
      const g = grounds(buildThemeStyle(THEMES[id]));
      const water = g.find((x) => x.name === "water")!;
      const key = [g[0].rgb, water.rgb].map((c) => c.map((v) => Math.round(v * 255)).join(",")).join("/");
      expect(seen.has(key), id).toBe(false);
      seen.add(key);
    }
  });

  it("authored themes define every palette role and apply it", () => {
    const keys: (keyof BasemapPalette)[] = [
      "background", "landuse", "green", "ice", "water", "waterway", "building", "buildingOutline", "aeroway",
      "motorway", "primary", "secondary", "minor", "path", "rail", "casing", "boundary",
      "label", "roadLabel", "waterLabel", "labelHalo",
    ];
    for (const id of THEME_IDS.filter((t) => THEMES[t].origin === "authored")) {
      const p = THEMES[id].palette!;
      for (const k of keys) expect(p[k], `${id}.${k}`).toBeTypeOf("string");
      const style = buildThemeStyle(THEMES[id]);
      const byRole = (role: string) => style.layers.filter((l) => layerRole(l) === role);
      expect(paintOf(byRole("background")[0])["background-color"]).toBe(p.background);
      for (const l of byRole("water")) expect(paintOf(l)["fill-color"]).toBe(p.water);
      for (const l of byRole("boundary")) expect(paintOf(l)["line-color"]).toBe(p.boundary);
      for (const l of byRole("label")) expect(paintOf(l)["text-halo-color"]).toBe(p.labelHalo);
      // Every road colour the style can produce comes from the palette.
      const tiers = new Set([p.motorway, p.primary, p.secondary, p.minor]);
      for (const l of byRole("road")) {
        for (const c of literals(paintOf(l)["line-color"]).filter((x) => typeof x === "string" && parseCssColor(x))) {
          expect(tiers.has(c as string), `${id} ${l.id}: ${String(c)}`).toBe(true);
        }
      }
    }
  });

  it("keeps the Latin-only wide-band labels on every theme", () => {
    for (const id of THEME_IDS) {
      const style = buildThemeStyle(THEMES[id]);
      const fields = style.layers.map((l) => JSON.stringify((l as { layout?: Paint }).layout?.["text-field"] ?? ""));
      const bilingual = fields.filter((f) => f.includes("name:nonlatin"));
      expect(bilingual.length).toBeGreaterThan(0);
      for (const f of bilingual) expect(f.startsWith('["step",["zoom"]')).toBe(true);
    }
  });

  it.each(THEME_IDS)("keeps every marker and overlay visible on %s", (id) => {
    const theme = THEMES[id];
    const g = grounds(buildThemeStyle(theme));
    const failures: string[] = [];
    for (const m of marks(theme.overlay)) {
      for (const ground of g) {
        const best = Math.max(...m.colors.map((c) => contrastRatio(c, ground.rgb)));
        if (best < MIN_CONTRAST) failures.push(`${m.name} on ${ground.name}: ${best.toFixed(2)}`);
      }
    }
    expect(failures).toEqual([]);
  });

  it.each(THEME_IDS)("keeps directions lines readable against their casing on %s", (id) => {
    // Roads come in every colour (Night Vision's are amber); the casing separates the line
    // from whatever road it runs along, so the line must stand out from the casing.
    const o = THEMES[id].overlay;
    for (const c of [o.routeAvoid, o.routeFast, o.routeCamera, o.highlight]) {
      expect(contrastRatio(rgb(c), rgb(o.routeCasing)), c).toBeGreaterThanOrEqual(MIN_CONTRAST);
    }
  });

  it("uses the overlay palette that matches the theme's ground", () => {
    for (const id of THEME_IDS) expect(THEMES[id].overlay.scheme).toBe(THEMES[id].scheme);
  });
});

describe("theme choice", () => {
  beforeEach(() => localStorage.clear());

  it("follows the OS until a theme is picked", () => {
    expect(loadThemeChoice()).toBeNull();
    const choice = effectiveChoice(loadThemeChoice(), DEFAULT_STYLE_URL);
    expect(choice).toBe("system");
    expect(resolveMapStyle(choice, DEFAULT_STYLE_URL, true).key).toBe("midnight");
    expect(resolveMapStyle(choice, DEFAULT_STYLE_URL, false).key).toBe("paper");
  });

  it("persists a picked theme", () => {
    saveThemeChoice("navy");
    expect(loadThemeChoice()).toBe("navy");
    expect(resolveMapStyle(effectiveChoice(loadThemeChoice(), DEFAULT_STYLE_URL), DEFAULT_STYLE_URL, false).key).toBe("navy");
  });

  it("falls back to the default when the saved theme no longer exists", () => {
    localStorage.setItem("flockfinder.mapTheme", "sepia");
    expect(loadThemeChoice()).toBeNull();
    expect(resolveMapStyle(effectiveChoice(loadThemeChoice(), DEFAULT_STYLE_URL), DEFAULT_STYLE_URL, true).key).toBe("midnight");
  });

  it("picks the overlay palette for a custom style from its background", () => {
    const style = (bg: string) => ({ version: 8, sources: {}, layers: [{ id: "bg", type: "background", paint: { "background-color": bg } }] }) as maplibregl.StyleSpecification;
    expect(overlayForStyle(style("rgba(173, 179, 191, 1)")).scheme).toBe("light");
    expect(overlayForStyle(style("#05080f")).scheme).toBe("dark");
    expect(overlayForStyle({ version: 8, sources: {}, layers: [] }).scheme).toBe("dark");
  });

  it("keeps a style URL chosen before themes existed", () => {
    const url = "https://example.org/style.json";
    expect(effectiveChoice(null, url)).toBe("custom");
    const r = resolveMapStyle("custom", url, false);
    expect(r.kind === "custom" && r.url).toBe(url);
    // A picked theme wins over the old URL.
    expect(effectiveChoice("paper", url)).toBe("paper");
  });
});
