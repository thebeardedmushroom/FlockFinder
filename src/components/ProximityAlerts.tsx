import { useEffect, useRef } from "react";
import { playChime } from "../lib/chime";
import { cameraKind, vendorLabel } from "../lib/classify";
import { cameraVisible } from "../lib/filters";
import { compassLabel, formatDistance, haversineM } from "../lib/geo";
import { api } from "../lib/ipc";
import { bearingDeg, checkProximity, newTracker, type ProximityAlert } from "../lib/proximity";
import { cameraKey, type Camera } from "../lib/types";
import { useAppStore } from "../store/useAppStore";

/** Reload the cameras around you after moving this far. */
const REFETCH_M = 250;
/** Cameras around you are loaded out to this many alert radii (at least 1 km). */
const LOOKAHEAD_FACTOR = 5;
const BANNER_MS = 12000;

function alertTitle(a: ProximityAlert): string {
  const what = cameraKind(a.camera) === "flock" ? "Flock camera" : "ALPR camera";
  return `${what} ${formatDistance(a.distanceM)} away`;
}

function alertBody(a: ProximityAlert): string {
  const direction = compassLabel(Math.round(a.bearing) % 360).split(" ")[0];
  const who = a.camera.tags.operator ?? vendorLabel(a.camera.tags);
  return [`to the ${direction}`, who, a.more > 0 ? `+${a.more} more nearby` : null].filter(Boolean).join(" · ");
}

/**
 * Live camera proximity alerts while the locate button is on. Positions are compared on this
 * device against cameras of the categories shown in Filters; nothing about the position is
 * stored. Renders the alert banner.
 */
export default function ProximityAlerts() {
  const pos = useAppStore((s) => s.userPosition);
  const locateActive = useAppStore((s) => s.locateActive);
  const proximity = useAppStore((s) => s.proximity);
  const alert = useAppStore((s) => s.proximityAlert);

  const tracker = useRef(newTracker());
  const nearby = useRef<Camera[]>([]);
  const lastFetch = useRef<{ lat: number; lon: number; radius: number } | null>(null);
  const fetching = useRef(false);

  // Cameras around you from the local store, independent of where the map is looking.
  useEffect(() => {
    if (!locateActive) {
      nearby.current = [];
      lastFetch.current = null;
      return;
    }
    if (!pos || !proximity.enabled || fetching.current) return;
    const radius = Math.max(1000, proximity.radiusM * LOOKAHEAD_FACTOR);
    const last = lastFetch.current;
    if (last && last.radius === radius && haversineM(last.lat, last.lon, pos.lat, pos.lon) < REFETCH_M) return;
    // Remember the attempt even if it fails (offline), so it is retried after moving, not on every fix.
    lastFetch.current = { lat: pos.lat, lon: pos.lon, radius };
    fetching.current = true;
    api
      .camerasNear(pos.lat, pos.lon, radius)
      .then((cams) => (nearby.current = cams))
      .catch(() => {
        /* offline or rate-limited: cameras already on the map still count */
      })
      .finally(() => (fetching.current = false));
  }, [pos, locateActive, proximity.enabled, proximity.radiusM]);

  // Check every new position.
  useEffect(() => {
    if (!pos || !proximity.enabled) return;
    const s = useAppStore.getState();
    const byKey = new Map<string, Camera>();
    for (const c of nearby.current) byKey.set(cameraKey(c), c);
    const targets = [...byKey.values()]
      .filter((c) => cameraVisible(c, s.filters))
      .map((c) => ({ key: cameraKey(c), lat: c.lat, lon: c.lon }));
    const hits = checkProximity(pos, targets, proximity.radiusM, tracker.current, Date.now());
    if (hits.length === 0) return;

    const camera = byKey.get(hits[0].key)!;
    const a: ProximityAlert = {
      camera,
      distanceM: hits[0].distanceM,
      bearing: bearingDeg(pos.lat, pos.lon, camera.lat, camera.lon),
      more: hits.length - 1,
      at: Date.now(),
    };
    s.setProximityAlert(a);
    s.setHighlighted(hits.map((h) => byKey.get(h.key)!));
    if (proximity.sound) {
      playChime();
      navigator.vibrate?.([200, 100, 200]);
    }
    if (document.visibilityState !== "visible" || !document.hasFocus()) {
      void api.notify(alertTitle(a), alertBody(a)).catch(() => {});
    }
  }, [pos, proximity]);

  // The banner clears itself after a while.
  useEffect(() => {
    if (!alert) return;
    const t = window.setTimeout(() => {
      const s = useAppStore.getState();
      s.setProximityAlert(null);
      s.setHighlighted([]);
    }, BANNER_MS);
    return () => window.clearTimeout(t);
  }, [alert]);

  // Keep the screen on while alerts can fire (phone on a mount). Released when locate is off.
  useEffect(() => {
    if (!locateActive || !proximity.enabled || !("wakeLock" in navigator)) return;
    let lock: WakeLockSentinel | null = null;
    let done = false;
    const acquire = async () => {
      try {
        lock = await navigator.wakeLock.request("screen");
      } catch {
        /* refused (battery saver etc.): the screen may sleep */
      }
    };
    const onVisible = () => {
      if (!done && document.visibilityState === "visible") void acquire();
    };
    void acquire();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      done = true;
      document.removeEventListener("visibilitychange", onVisible);
      void lock?.release().catch(() => {});
    };
  }, [locateActive, proximity.enabled]);

  if (!alert) return null;
  const dismiss = () => {
    const s = useAppStore.getState();
    s.setProximityAlert(null);
    s.setHighlighted([]);
  };
  return (
    <div className={`proximity-alert ${cameraKind(alert.camera)}`} role="alert">
      <span className="icon" aria-hidden>
        ⚠
      </span>
      <div className="grow">
        <div className="title">{alertTitle(alert)}</div>
        <div className="sub">{alertBody(alert)}</div>
      </div>
      <button className="btn small" onClick={() => useAppStore.getState().select({ kind: "camera", camera: alert.camera })}>
        Details
      </button>
      <button className="close" onClick={dismiss} aria-label="Dismiss">
        ×
      </button>
    </div>
  );
}
