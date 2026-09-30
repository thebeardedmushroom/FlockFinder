import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";

/** Panels are bottom sheets at this width (the same query as in styles.css). */
const SHEET_QUERY = "(max-width: 720px)";
/** Vertical movement before a press on the header becomes a drag (so taps still click). */
const DRAG_START_PX = 6;
/** A release this fast (px per ms) settles in the direction of the flick… */
const FLICK_SPEED = 0.45;
/** …judged over the movement of the last this many ms. */
const FLICK_WINDOW_MS = 90;
/** How long the settle animation runs (matches the transition in styles.css). */
const SETTLE_MS = 240;

interface Drag {
  id: number;
  y0: number;
  /** Offset when the drag started (0 expanded, `span` collapsed). */
  base: number;
  span: number;
  active: boolean;
  /** Recent (time, y) samples, for the release speed. */
  samples: [number, number][];
  /** A press that started on a button (the close ×, the grab bar). */
  onButton: boolean;
}

/**
 * A panel that is a bottom sheet on a phone: drag its header down to collapse it to just the
 * header (the map gets the room), drag it up or tap the header to bring it back, or flick
 * either way. The grab bar is a button too (keyboard and screen readers). Nothing changes on
 * wider screens, where panels are side panels.
 *
 * `resetKey`: when it changes, the sheet opens again (a new selection in the same panel).
 */
export function useSheet(resetKey?: unknown) {
  const ref = useRef<HTMLDivElement>(null);
  const [collapsed, setCollapsed] = useState(false);
  const drag = useRef<Drag | null>(null);
  /** The last press became a drag: the click it ends with must not press a button. */
  const moved = useRef(false);
  const settleTimer = useRef<number | null>(null);
  /** Removes the window listeners of a press in progress. */
  const release = useRef<(() => void) | null>(null);

  useEffect(() => setCollapsed(false), [resetKey]);
  useEffect(
    () => () => {
      if (settleTimer.current) window.clearTimeout(settleTimer.current);
      release.current?.();
    },
    [],
  );

  const header = () => ref.current?.querySelector<HTMLElement>(".panel-header") ?? null;
  /** How far the sheet moves down when collapsed: everything but the header. */
  const span = () => Math.max(0, (ref.current?.offsetHeight ?? 0) - (header()?.offsetHeight ?? 0));

  /** Settle to collapsed or open, animating from wherever a drag left it. */
  const settle = (collapse: boolean) => {
    const el = ref.current;
    if (!el) return;
    el.style.setProperty("--sheet-peek", `${header()?.offsetHeight ?? 48}px`);
    el.style.transition = "";
    // Animate to the target with an inline value first, so the switch of class that follows
    // has nothing left to move (no jump between the two).
    el.style.transform = collapse ? `translateY(${span()}px)` : "translateY(0px)";
    setCollapsed(collapse);
    if (settleTimer.current) window.clearTimeout(settleTimer.current);
    settleTimer.current = window.setTimeout(() => {
      el.style.transform = "";
    }, SETTLE_MS);
  };

  const onMove = (e: PointerEvent) => {
    const g = drag.current;
    const el = ref.current;
    if (!g || g.id !== e.pointerId || !el) return;
    const dy = e.clientY - g.y0;
    if (!g.active) {
      if (Math.abs(dy) < DRAG_START_PX) return;
      g.active = true;
      moved.current = true;
      el.style.transition = "none";
    }
    e.preventDefault();
    el.style.transform = `translateY(${Math.min(g.span, Math.max(0, g.base + dy))}px)`;
    g.samples.push([e.timeStamp, e.clientY]);
    while (g.samples.length > 2 && e.timeStamp - g.samples[0][0] > FLICK_WINDOW_MS) g.samples.shift();
  };

  const onEnd = (e: PointerEvent) => {
    const g = drag.current;
    if (!g || g.id !== e.pointerId) return;
    drag.current = null;
    release.current?.();
    if (!g.active) {
      // A tap on the header (not on one of its buttons) toggles the sheet.
      if (e.type === "pointerup" && !g.onButton) settle(!collapsed);
      return;
    }
    const [t0, y0] = g.samples[0];
    const speed = (e.clientY - y0) / Math.max(1, e.timeStamp - t0);
    const y = Math.min(g.span, Math.max(0, g.base + e.clientY - g.y0));
    const collapse = speed > FLICK_SPEED ? true : speed < -FLICK_SPEED ? false : y > g.span / 2;
    settle(e.type === "pointercancel" ? collapsed : collapse);
  };

  // The rest of a press is followed on the window: a fast drag leaves the header at once.
  const onPointerDown = (e: ReactPointerEvent<HTMLElement>) => {
    moved.current = false;
    if (!window.matchMedia(SHEET_QUERY).matches) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest("input, select, textarea")) return;
    release.current?.();
    const s = span();
    drag.current = {
      id: e.pointerId,
      y0: e.clientY,
      base: collapsed ? s : 0,
      span: s,
      active: false,
      samples: [[e.timeStamp, e.clientY]],
      onButton: !!target.closest("button"),
    };
    window.addEventListener("pointermove", onMove, { passive: false });
    window.addEventListener("pointerup", onEnd);
    window.addEventListener("pointercancel", onEnd);
    release.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onEnd);
      window.removeEventListener("pointercancel", onEnd);
      release.current = null;
    };
  };

  /** A drag that started on a header button (the close ×) must not also press it. */
  const onClickCapture = (e: ReactMouseEvent) => {
    if (moved.current) {
      e.stopPropagation();
      e.preventDefault();
      moved.current = false;
    }
  };

  const grip = (
    <button
      type="button"
      className="sheet-grip"
      onClick={() => settle(!collapsed)}
      aria-label={collapsed ? "Expand panel" : "Collapse panel"}
      aria-expanded={!collapsed}
    />
  );

  return {
    ref,
    collapsed,
    /** Class for the panel element. */
    className: collapsed ? "sheet-collapsed" : "",
    /** Spread on the panel's header. */
    headerProps: { onPointerDown, onClickCapture },
    /** The grab bar, first thing in the header. */
    grip,
  };
}
