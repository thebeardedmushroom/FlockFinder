import { cameraKind, type MarkerKind } from "./classify";
import type { Camera, Submission, WifiSighting } from "./types";

/**
 * Filter panel state: one toggle per marker category plus a free-text operator filter.
 * `wifi` is the heuristic Wi-Fi fingerprint layer (suspected devices).
 */
export interface FilterState {
  flock: boolean;
  alpr: boolean;
  user: boolean;
  wifi: boolean;
  operator: string;
  /** Draw direction cones. A display option, not a filter: it hides no markers. */
  cones: boolean;
}

export const initialFilters: FilterState = {
  flock: true,
  alpr: true,
  user: true,
  wifi: true,
  operator: "",
  cones: true,
};

export const ALL_KINDS: MarkerKind[] = ["flock", "alpr", "user", "wifi"];

export type FilterAction =
  | { type: "toggle"; kind: MarkerKind }
  | { type: "set"; kind: MarkerKind; value: boolean }
  | { type: "solo"; kind: MarkerKind }
  | { type: "operator"; text: string }
  | { type: "cones"; value: boolean }
  | { type: "reset" };

export function filterReducer(state: FilterState, action: FilterAction): FilterState {
  switch (action.type) {
    case "toggle":
      return { ...state, [action.kind]: !state[action.kind] };
    case "set":
      return { ...state, [action.kind]: action.value };
    case "solo": {
      const next = { ...state };
      for (const k of ALL_KINDS) next[k] = k === action.kind;
      return next;
    }
    case "operator":
      return { ...state, operator: action.text };
    case "cones":
      return { ...state, cones: action.value };
    case "reset":
      return { ...initialFilters };
    default:
      return state;
  }
}

function operatorMatches(operator: string | null | undefined, needle: string): boolean {
  const n = needle.trim().toLowerCase();
  if (n === "") return true;
  return (operator ?? "").toLowerCase().includes(n);
}

export function cameraVisible(camera: Camera, f: FilterState): boolean {
  if (!f[cameraKind(camera)]) return false;
  return operatorMatches(camera.tags.operator, f.operator);
}

export function submissionVisible(sub: Submission, f: FilterState): boolean {
  if (!f.user) return false;
  return operatorMatches(sub.operator, f.operator);
}

/** Wi-Fi sightings carry no operator, so any operator text hides them. */
export function sightingVisible(_s: WifiSighting, f: FilterState): boolean {
  return f.wifi && f.operator.trim() === "";
}

/** Number of ways the current filters deviate from "show everything". */
export function activeFilterCount(f: FilterState): number {
  let n = 0;
  for (const k of ALL_KINDS) if (!f[k]) n++;
  if (f.operator.trim() !== "") n++;
  return n;
}
