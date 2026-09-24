/**
 * Colour for camera data. One accent hue whose lightness carries magnitude, on a
 * perceptually uniform ramp (OKLCH: equal steps in t are equal steps in perceived
 * lightness). No rainbow: a hue change would invent category boundaries the data lacks.
 * A second hue marks state (unverified submissions), never magnitude.
 */
export type Rgb = [number, number, number];

export const ACCENT_HUE = 200;
/** Unverified submissions. */
export const STATE_HUE = 300;

const L0 = 0.36;
const L1 = 0.95;

function linearToSrgb(x: number): number {
  return x <= 0.0031308 ? 12.92 * x : 1.055 * Math.pow(x, 1 / 2.4) - 0.055;
}

/** OKLCH → sRGB in [0, 1], or null when the colour is outside the sRGB gamut. */
export function oklchToRgb(L: number, C: number, hueDeg: number): Rgb | null {
  const h = (hueDeg * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const rgb: Rgb = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
  if (rgb.some((v) => v < -1e-4 || v > 1 + 1e-4)) return null;
  return rgb.map((v) => linearToSrgb(Math.max(0, Math.min(1, v)))) as Rgb;
}

/** OKLCH → sRGB, reducing chroma (keeping lightness and hue) until it fits the gamut. */
export function oklchInGamut(L: number, C: number, hueDeg: number): Rgb {
  let lo = 0;
  let hi = C;
  let best = oklchToRgb(L, 0, hueDeg)!;
  if (oklchToRgb(L, C, hueDeg)) return oklchToRgb(L, C, hueDeg)!;
  for (let i = 0; i < 18; i++) {
    const mid = (lo + hi) / 2;
    const c = oklchToRgb(L, mid, hueDeg);
    if (c) {
      best = c;
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return best;
}

export function rampLightness(t: number): number {
  return L0 + (L1 - L0) * Math.max(0, Math.min(1, t));
}

function rampChroma(t: number): number {
  // Most colourful in the middle; near-neutral at the dim end, pale at the bright end.
  const u = 2 * Math.max(0, Math.min(1, t)) - 1;
  return 0.055 + 0.085 * (1 - u * u);
}

const LUT_SIZE = 256;
const LUT: Rgb[] = Array.from({ length: LUT_SIZE }, (_, i) => {
  const t = i / (LUT_SIZE - 1);
  return oklchInGamut(rampLightness(t), rampChroma(t), ACCENT_HUE);
});

/** Ramp colour for t in [0, 1]. */
export function rampRgb(t: number): Rgb {
  const i = Math.round(Math.max(0, Math.min(1, t)) * (LUT_SIZE - 1));
  return LUT[i];
}

export function rgbCss(c: Rgb, alpha = 1): string {
  const [r, g, b] = c.map((v) => Math.round(v * 255));
  return alpha >= 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export const rampCss = (t: number, alpha = 1): string => rgbCss(rampRgb(t), alpha);

/** The accent at full strength: individual cameras and UI marks for camera data. */
export const ACCENT_RGB: Rgb = rampRgb(0.78);
export const ACCENT_CSS = rgbCss(ACCENT_RGB);
/** Second hue: unverified submissions only. */
export const STATE_RGB: Rgb = oklchInGamut(0.72, 0.16, STATE_HUE);
export const STATE_CSS = rgbCss(STATE_RGB);

/** CSS gradient of the ramp (legend bar). */
export function rampGradientCss(stops = 9): string {
  const parts: string[] = [];
  for (let i = 0; i < stops; i++) {
    const t = i / (stops - 1);
    parts.push(`${rampCss(t)} ${(t * 100).toFixed(1)}%`);
  }
  return `linear-gradient(90deg, ${parts.join(", ")})`;
}

/** Stops for a MapLibre `interpolate` expression over t. */
export function rampStops(stops = 9, alpha = 1): (number | string)[] {
  const out: (number | string)[] = [];
  for (let i = 0; i < stops; i++) {
    const t = i / (stops - 1);
    out.push(t, rampCss(t, alpha));
  }
  return out;
}

/** WCAG relative luminance, for choosing a label colour that reads on a fill. */
export function relativeLuminance(c: Rgb): number {
  const lin = c.map((v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}
