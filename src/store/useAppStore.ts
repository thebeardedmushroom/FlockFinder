import { create } from "zustand";
import { filterReducer, initialFilters, type FilterAction, type FilterState } from "../lib/filters";
import {
  loadProximitySettings,
  saveProximitySettings,
  type ProximityAlert,
  type ProximitySettings,
  type UserPosition,
} from "../lib/proximity";
import type { ViewCounts } from "../lib/clusterEngine";
import type {
  AlertState,
  AppInfo,
  BBox,
  Camera,
  Settings,
  Submission,
  SyncStatus,
  ViewState,
  WifiSighting,
} from "../lib/types";
import type { LodState } from "../map/cameraLayer";

/** The installed camera snapshot (the arrays live in lib/dataset.ts). */
export interface DatasetInfo {
  count: number;
  /** Changes whenever a new snapshot is installed. */
  version: number;
  bytes: number;
  fetchMs: number;
  decodeMs: number;
}

export type Panel = "none" | "filters" | "submissions" | "alerts" | "settings";
export type Mode = "view" | "add" | "draw";

export type Selection =
  | { kind: "camera"; camera: Camera }
  | { kind: "submission"; submission: Submission }
  | { kind: "wifi"; sighting: WifiSighting }
  | null;

export interface Toast {
  id: number;
  text: string;
  tone: "info" | "warn" | "error" | "success";
  action?: { label: string; run: () => void };
}

export interface FlyRequest {
  lat?: number;
  lon?: number;
  zoom?: number;
  bbox?: BBox;
  seq: number;
}

/** A pending "edit this submission" or "new submission at this pin" form. */
export interface SubmissionDraft {
  id: number | null;
  lat: number;
  lon: number;
  existing: Submission | null;
}

export interface PendingWatchArea {
  lat: number;
  lon: number;
}

export interface PendingRoute {
  points: [number, number][];
  name: string;
  lengthM: number | null;
  source: "draw" | "gpx";
}

interface AppStore {
  info: AppInfo | null;
  settings: Settings | null;
  view: ViewState | null;
  zoom: number;
  bbox: BBox | null;

  dataset: DatasetInfo | null;
  sync: SyncStatus | null;
  /** Level of detail the camera layer is showing. */
  lod: LodState | null;
  /** Camera counts inside the viewport (filtered and not). */
  inView: ViewCounts | null;
  /** The hex cell under the pointer (wide band). */
  hexHover: { count: number; users: number } | null;
  /** Wi-Fi fingerprint sightings in view, keyed by BSSID. */
  sightings: Record<string, WifiSighting>;
  submissions: Submission[];
  filters: FilterState;
  selection: Selection;
  mode: Mode;
  draftPin: { lat: number; lon: number } | null;
  drawPoints: [number, number][];

  offline: boolean;
  panel: Panel;
  alertState: AlertState | null;
  /** Cameras drawn with a highlight ring (e.g. "show new cameras on map"). */
  highlighted: Camera[];
  fly: FlyRequest | null;
  firstRunOpen: boolean;
  toasts: Toast[];

  submissionDraft: SubmissionDraft | null;
  pendingWatchArea: PendingWatchArea | null;
  pendingRoute: PendingRoute | null;
  uploadTarget: Submission | null;
  contextMenu: { x: number; y: number; lat: number; lon: number } | null;

  /** Live position while the locate button is on; null otherwise. Never persisted. */
  userPosition: UserPosition | null;
  /** True while the locate control is on (following you or not). */
  locateActive: boolean;
  /** Camera proximity alert preferences (per device, localStorage). */
  proximity: ProximitySettings;
  /** The proximity alert currently shown in the banner. */
  proximityAlert: ProximityAlert | null;

  setInfo(info: AppInfo): void;
  setSettings(settings: Settings): void;
  setView(view: ViewState): void;
  setViewport(bbox: BBox, zoom: number): void;
  setDataset(d: DatasetInfo): void;
  setSync(s: SyncStatus): void;
  setLod(l: LodState): void;
  setInView(c: ViewCounts): void;
  setHexHover(h: AppStore["hexHover"]): void;
  setSightings(sightings: WifiSighting[]): void;
  setSubmissions(subs: Submission[]): void;
  dispatchFilter(action: FilterAction): void;
  select(sel: Selection): void;
  setMode(mode: Mode): void;
  setDraftPin(pin: { lat: number; lon: number } | null): void;
  addDrawPoint(p: [number, number]): void;
  undoDrawPoint(): void;
  clearDraw(): void;
  setOffline(offline: boolean): void;
  setPanel(panel: Panel): void;
  togglePanel(panel: Panel): void;
  setAlertState(state: AlertState | null): void;
  setHighlighted(cameras: Camera[]): void;
  flyTo(req: Omit<FlyRequest, "seq">): void;
  setFirstRunOpen(open: boolean): void;
  pushToast(text: string, tone?: Toast["tone"], action?: Toast["action"]): void;
  dismissToast(id: number): void;
  setSubmissionDraft(d: SubmissionDraft | null): void;
  setPendingWatchArea(p: PendingWatchArea | null): void;
  setPendingRoute(p: PendingRoute | null): void;
  setUploadTarget(s: Submission | null): void;
  setContextMenu(m: AppStore["contextMenu"]): void;
  setUserPosition(p: UserPosition | null): void;
  setLocateActive(on: boolean): void;
  setProximity(p: Partial<ProximitySettings>): void;
  setProximityAlert(a: ProximityAlert | null): void;
}

let toastSeq = 0;
let flySeq = 0;

export const useAppStore = create<AppStore>((set, get) => ({
  info: null,
  settings: null,
  view: null,
  zoom: 0,
  bbox: null,
  dataset: null,
  sync: null,
  lod: null,
  inView: null,
  hexHover: null,
  sightings: {},
  submissions: [],
  filters: initialFilters,
  selection: null,
  mode: "view",
  draftPin: null,
  drawPoints: [],
  offline: false,
  panel: "none",
  alertState: null,
  highlighted: [],
  fly: null,
  firstRunOpen: false,
  toasts: [],
  submissionDraft: null,
  pendingWatchArea: null,
  pendingRoute: null,
  uploadTarget: null,
  contextMenu: null,
  userPosition: null,
  locateActive: false,
  proximity: loadProximitySettings(),
  proximityAlert: null,

  setInfo: (info) => set({ info }),
  setSettings: (settings) => set({ settings }),
  setView: (view) => set({ view }),
  setViewport: (bbox, zoom) => set({ bbox, zoom }),
  setDataset: (dataset) => set({ dataset }),
  setSync: (sync) => set({ sync }),
  setLod: (lod) => set({ lod }),
  setInView: (inView) => set({ inView }),
  setHexHover: (hexHover) => {
    const cur = get().hexHover;
    if (cur?.count === hexHover?.count && cur?.users === hexHover?.users) return;
    set({ hexHover });
  },
  setSightings: (sightings) => {
    const map: Record<string, WifiSighting> = {};
    for (const s of sightings) map[s.netid] = s;
    set({ sightings: map });
  },
  setSubmissions: (submissions) => set({ submissions }),
  dispatchFilter: (action) => set({ filters: filterReducer(get().filters, action) }),
  select: (selection) => set({ selection }),
  setMode: (mode) =>
    set({
      mode,
      draftPin: mode === "add" ? get().draftPin : null,
      drawPoints: mode === "draw" ? get().drawPoints : [],
      contextMenu: null,
    }),
  setDraftPin: (draftPin) => set({ draftPin }),
  addDrawPoint: (p) => set({ drawPoints: [...get().drawPoints, p] }),
  undoDrawPoint: () => set({ drawPoints: get().drawPoints.slice(0, -1) }),
  clearDraw: () => set({ drawPoints: [] }),
  setOffline: (offline) => set({ offline }),
  setPanel: (panel) => set({ panel, contextMenu: null }),
  togglePanel: (panel) => set({ panel: get().panel === panel ? "none" : panel, contextMenu: null }),
  setAlertState: (alertState) => set({ alertState }),
  setHighlighted: (highlighted) => set({ highlighted }),
  flyTo: (req) => set({ fly: { ...req, seq: ++flySeq } }),
  setFirstRunOpen: (firstRunOpen) => set({ firstRunOpen }),
  pushToast: (text, tone = "info", action) => {
    const id = ++toastSeq;
    set({ toasts: [...get().toasts, { id, text, tone, action }] });
    const ttl = tone === "error" ? 12000 : action ? 15000 : 6000;
    setTimeout(() => get().dismissToast(id), ttl);
  },
  dismissToast: (id) => set({ toasts: get().toasts.filter((t) => t.id !== id) }),
  setSubmissionDraft: (submissionDraft) => set({ submissionDraft }),
  setPendingWatchArea: (pendingWatchArea) => set({ pendingWatchArea, contextMenu: null }),
  setPendingRoute: (pendingRoute) => set({ pendingRoute }),
  setUploadTarget: (uploadTarget) => set({ uploadTarget }),
  setContextMenu: (contextMenu) => set({ contextMenu }),
  setUserPosition: (userPosition) => set({ userPosition }),
  // Turning locate off also drops the last known position.
  setLocateActive: (locateActive) => set(locateActive ? { locateActive } : { locateActive, userPosition: null }),
  setProximity: (p) => {
    const proximity = { ...get().proximity, ...p };
    saveProximitySettings(proximity);
    set({ proximity });
  },
  setProximityAlert: (proximityAlert) => set({ proximityAlert }),
}));
