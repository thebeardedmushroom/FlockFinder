// The camera snapshot currently shown on the map. Kept outside the store: it is large, and
// only the map layer reads the arrays. The store carries a version number that changes
// whenever a new snapshot is installed.
import { useAppStore } from "../store/useAppStore";
import { toastError } from "./actions";
import { decodePoints, EMPTY_POINTS, type CameraPoints } from "./cameraData";
import { api } from "./ipc";

let current: CameraPoints = EMPTY_POINTS;
let loading: Promise<void> | null = null;
let again = false;

export const getPoints = (): CameraPoints => current;

async function loadOnce(): Promise<void> {
  const t0 = performance.now();
  const raw = await api.getCameraPoints();
  const buf = raw instanceof ArrayBuffer ? raw : new Uint8Array(raw as unknown as number[]).buffer;
  const t1 = performance.now();
  current = decodePoints(buf);
  const store = useAppStore.getState();
  store.setDataset({
    count: current.count,
    version: (store.dataset?.version ?? 0) + 1,
    bytes: buf.byteLength,
    fetchMs: t1 - t0,
    decodeMs: performance.now() - t1,
  });
  performance.mark("ff:points-decoded");
}

/** Load (or reload) the snapshot. Overlapping calls coalesce into one follow-up load. */
export function loadDataset(): Promise<void> {
  if (loading) {
    again = true;
    return loading;
  }
  loading = (async () => {
    try {
      do {
        again = false;
        await loadOnce();
      } while (again);
    } catch (e) {
      toastError(e, "Could not load cameras");
    } finally {
      loading = null;
    }
  })();
  return loading;
}
