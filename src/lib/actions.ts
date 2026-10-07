// Shared orchestration used by several components.
import { useAppStore } from "../store/useAppStore";
import { api } from "./ipc";
import { bboxOf, haversineM } from "./geo";
import { errorMessage, isAppError, type Camera } from "./types";

export function toastError(e: unknown, prefix?: string): void {
  const msg = errorMessage(e);
  const store = useAppStore.getState();
  if (isAppError(e) && e.kind === "cancelled") return;
  if (isAppError(e) && e.kind === "offline") {
    store.setOffline(true);
    store.pushToast(prefix ? `${prefix}: offline — ${msg}` : `Offline — ${msg}`, "warn");
    return;
  }
  store.pushToast(prefix ? `${prefix}: ${msg}` : msg, "error");
}

export async function reloadSubmissions(): Promise<void> {
  try {
    useAppStore.getState().setSubmissions(await api.listSubmissions());
  } catch (e) {
    toastError(e, "Could not load submissions");
  }
}

export async function reloadPlaces(): Promise<void> {
  try {
    useAppStore.getState().setSavedPlaces(await api.listSavedPlaces());
  } catch (e) {
    toastError(e, "Could not load saved places");
  }
}

export async function reloadAlertState(): Promise<void> {
  try {
    useAppStore.getState().setAlertState(await api.getAlertState());
  } catch (e) {
    toastError(e, "Could not load alerts");
  }
}

export async function reloadSettings(): Promise<void> {
  try {
    useAppStore.getState().setSettings(await api.getSettings());
  } catch (e) {
    toastError(e, "Could not load settings");
  }
}

/** Highlight cameras on the map and fly to fit them. */
export function showCamerasOnMap(cameras: Camera[]): void {
  const store = useAppStore.getState();
  if (cameras.length === 0) {
    store.pushToast("No cameras to show for this target yet.", "info");
    return;
  }
  store.setHighlighted(cameras);
  const bbox = bboxOf(cameras.map((c) => [c.lat, c.lon]));
  if (!bbox) return;
  if (cameras.length === 1 || haversineM(bbox.south, bbox.west, bbox.north, bbox.east) < 200) {
    store.flyTo({ lat: cameras[0].lat, lon: cameras[0].lon, zoom: 16 });
  } else {
    store.flyTo({ bbox });
  }
}

export async function showCameraKeysOnMap(keys: string[]): Promise<void> {
  try {
    showCamerasOnMap(await api.getCamerasByKeys(keys));
  } catch (e) {
    toastError(e);
  }
}

export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    useAppStore.getState().pushToast("Copied to clipboard.", "success");
  } catch {
    useAppStore.getState().pushToast(`Copy failed; value: ${text}`, "warn");
  }
}

/** Run the worldwide camera sync now and report the result. */
export async function runSync(): Promise<void> {
  const store = useAppStore.getState();
  try {
    const o = await api.syncNow();
    if (o.unchanged) {
      store.pushToast("Camera data is already up to date.", "success");
      return;
    }
    store.pushToast(
      `Synced ${o.elements.toLocaleString("en-US")} cameras${o.marked_stale > 0 ? ` (${o.marked_stale} no longer in OSM, shown hollow)` : ""}.`,
      "success",
    );
  } catch (e) {
    toastError(e, "Camera sync failed");
  }
}

/** Wi-Fi sightings changed on disk (download, import, clear): reload them for the view. */
export const REFRESH_AREA_EVENT = "flockfinder:refresh-area";
export function requestAreaRefresh(): void {
  window.dispatchEvent(new CustomEvent(REFRESH_AREA_EVENT));
}

export async function openOsm(url: string): Promise<void> {
  try {
    await api.openExternal(url);
  } catch (e) {
    toastError(e, "Could not open browser");
  }
}
