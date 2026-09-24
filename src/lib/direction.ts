import { compassLabel } from "./geo";

/** Cone width (degrees) for a bare bearing. DeFlock writes 45°-wide ranges, so match it. */
export const DEFAULT_FOV = 45;
/** Cones drawn per camera; extra values in a `;` list are ignored. */
export const MAX_CONES = 6;

export interface Cone {
  /** Centre of the field of view, degrees clockwise from north, in [0, 360). */
  bearing: number;
  /** Angular width, degrees. */
  width: number;
}

const CARDINALS = ["N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW"];

const norm = (deg: number) => ((deg % 360) + 360) % 360;

function parseBearing(s: string): number | null {
  const t = s.trim();
  if (/^-?\d+(\.\d+)?$/.test(t)) return norm(Number(t));
  const i = CARDINALS.indexOf(t.toUpperCase());
  return i >= 0 ? i * 22.5 : null;
}

/**
 * Parse an OSM `direction` value. Accepts degrees (`75`, `-90`), cardinal letters (`NE`),
 * `;`-separated lists (one cone each) and clockwise ranges (`338-23` is a 45° field centred
 * on north). Unparseable parts are skipped; duplicates are dropped.
 */
export function parseDirectionValue(raw: string): Cone[] {
  const cones: Cone[] = [];
  for (const part of raw.split(";")) {
    const p = part.trim();
    if (p === "") continue;
    let cone: Cone | null = null;
    const range = /^(.+?)\s*-\s*(.+)$/.exec(p);
    if (range) {
      const a = parseBearing(range[1]);
      const b = parseBearing(range[2]);
      if (a !== null && b !== null) {
        const width = norm(b - a);
        cone = width === 0 ? { bearing: a, width: DEFAULT_FOV } : { bearing: norm(a + width / 2), width };
      }
    } else {
      const a = parseBearing(p);
      if (a !== null) cone = { bearing: a, width: DEFAULT_FOV };
    }
    if (cone && !cones.some((c) => Math.abs(c.bearing - cone.bearing) < 0.5 && c.width === cone.width)) {
      cones.push(cone);
      if (cones.length === MAX_CONES) break;
    }
  }
  return cones;
}

/** Cones for an OSM element, from `direction` or, failing that, `camera:direction`. */
export function parseDirections(tags: Record<string, string | undefined>): Cone[] {
  const raw = tags.direction ?? tags["camera:direction"];
  return raw ? parseDirectionValue(raw) : [];
}

/**
 * Human-readable direction tag: `E (90°)`; lists and ranges keep the raw value alongside
 * (`E (90°), W (270°) · 90;270`). Unparseable values are returned unchanged.
 */
export function formatDirection(raw: string): string {
  const cones = parseDirectionValue(raw);
  if (cones.length === 0) return raw;
  const labels = cones.map((c) => compassLabel(Math.round(c.bearing) % 360)).join(", ");
  return /^\d+$/.test(raw.trim()) ? labels : `${labels} · ${raw}`;
}
