import { useEffect, useRef, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { playChime } from "../lib/chime";
import { cameraKind, vendorLabel } from "../lib/classify";
import { cameraVisible } from "../lib/filters";
import { compassLabel, formatDistance, haversineM } from "../lib/geo";
import { api } from "../lib/ipc";
import { navCovered } from "../lib/nav";
import { bearingDeg, checkProximity, newTracker, type ProximityAlert } from "../lib/proximity";
import { cameraKey, type Camera } from "../lib/types";
import { useAppStore } from "../store/useAppStore";

/** Reload the cameras around you after moving this far. */
const REFETCH_M = 250;
/** Cameras around you are loaded out to this many alert radii (at least 1 km). */
const LOOKAHEAD_FACTOR = 5;
/** Horizontal movement before a press on the banner becomes a swipe (so taps still click). */
const SWIPE_START_PX = 8;
/** A swipe dismisses once it covers this share of the banner's width, or flicks fast enough. */
const SWIPE_DISMISS_SHARE = 0.3;
const SWIPE_DISMISS_SPEED = 0.6; // px per ms
const SWIPE_ANIM_MS = 180;

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
  // While navigating, positions come from the navigation session.
  const locateActive = useAppStore((s) => s.locateActive || s.nav !== null);
  const navigating = useAppStore((s) => s.nav !== null);
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
    // Cameras on the route are navigation's to announce (once, ahead of time); only others here.
    const targets = [...byKey.values()]
      .filter((c) => cameraVisible(c, s.filters))
      .filter((c) => !(navigating && navCovered.has(cameraKey(c))))
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
  }, [pos, proximity, navigating]);

  // The banner stays until it is swiped away, closed, or its Details are opened. A newer
  // alert replaces it in place; reset any swipe that was in progress on the old one.
  const bannerRef = useRef<HTMLDivElement>(null);
  const swipe = useRef<{ id: number; x0: number; y0: number; t0: number; dx: number; active: boolean; moved: boolean } | null>(null);
  useEffect(() => {
    const el = bannerRef.current;
    if (!el) return;
    el.style.transition = "";
    el.style.translate = "";
    el.style.opacity = "";
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
  const openDetails = () => {
    const camera = alert.camera;
    dismiss();
    useAppStore.getState().select({ kind: "camera", camera });
  };

  const setOffset = (dx: number, width: number) => {
    const el = bannerRef.current!;
    el.style.translate = `${dx}px 0`;
    el.style.opacity = String(Math.max(0.25, 1 - Math.abs(dx) / width));
  };
  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    swipe.current = { id: e.pointerId, x0: e.clientX, y0: e.clientY, t0: e.timeStamp, dx: 0, active: false, moved: false };
    bannerRef.current!.style.transition = "none";
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const g = swipe.current;
    if (!g || g.id !== e.pointerId) return;
    const dx = e.clientX - g.x0;
    if (!g.active) {
      if (Math.abs(dx) < SWIPE_START_PX || Math.abs(dx) < Math.abs(e.clientY - g.y0)) return;
      g.active = true;
      g.moved = true;
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        /* pointer already gone: the swipe still tracks while it stays over the banner */
      }
    }
    g.dx = dx;
    setOffset(dx, e.currentTarget.offsetWidth);
  };
  const onPointerEnd = (e: ReactPointerEvent<HTMLDivElement>) => {
    const g = swipe.current;
    if (!g || g.id !== e.pointerId) return;
    const el = e.currentTarget;
    const width = el.offsetWidth;
    const speed = Math.abs(g.dx) / Math.max(1, e.timeStamp - g.t0);
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    el.style.transition = reduced ? "none" : `translate ${SWIPE_ANIM_MS}ms ease-out, opacity ${SWIPE_ANIM_MS}ms ease-out`;
    if (g.active && e.type === "pointerup" && (Math.abs(g.dx) > width * SWIPE_DISMISS_SHARE || speed > SWIPE_DISMISS_SPEED)) {
      el.style.translate = `${Math.sign(g.dx) * (width + 40)}px 0`;
      el.style.opacity = "0";
      window.setTimeout(dismiss, reduced ? 0 : SWIPE_ANIM_MS);
    } else {
      el.style.translate = "";
      el.style.opacity = "";
    }
    g.active = false;
  };
  // A swipe that started on a button must not also press it.
  const onClickCapture = (e: ReactMouseEvent) => {
    if (swipe.current?.moved) {
      e.stopPropagation();
      e.preventDefault();
    }
    swipe.current = null;
  };

  return (
    <div
      ref={bannerRef}
      className={`proximity-alert ${cameraKind(alert.camera)}`}
      role="alert"
      title="Swipe sideways to dismiss"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerEnd}
      onPointerCancel={onPointerEnd}
      onClickCapture={onClickCapture}
    >
      <span className="icon" aria-hidden>
        ⚠
      </span>
      <div className="grow">
        <div className="title">{alertTitle(alert)}</div>
        <div className="sub">{alertBody(alert)}</div>
      </div>
      <button className="btn small" onClick={openDetails}>
        Details
      </button>
      <button className="close" onClick={dismiss} aria-label="Dismiss">
        ×
      </button>
    </div>
  );
}
