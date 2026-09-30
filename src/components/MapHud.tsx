// Map-corner readouts for the camera layer: the HUD (in view / total / last sync), the
// legend for the aggregate bands, and the empty-viewport states.
import { useEffect, useState } from "react";
import { runSync } from "../lib/actions";
import { bandHasLegend, formatCount, formatExact, nodeRadius, type Band } from "../lib/lod";
import { rampGradientCss, rampCss, STATE_CSS } from "../lib/ramp";
import { DARK_OVERLAY } from "../map/themes";
import { useMapStyle } from "../map/useMapStyle";
import { useAppStore } from "../store/useAppStore";

const BAND_NAME: Record<Band, string> = { wide: "WIDE", mid: "MID", near: "NEAR", detail: "DETAIL" };

function syncStamp(t: number | null): string {
  if (!t) return "never";
  const d = new Date(t * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function mb(bytes: number): string {
  return `${(bytes / 1_048_576).toFixed(1)} MB`;
}

/** Phone-sized: the readouts drop to their short forms so each stays one line. */
function useNarrow(): boolean {
  const [narrow, setNarrow] = useState(() => window.matchMedia("(max-width: 720px)").matches);
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 720px)");
    const onChange = (e: MediaQueryListEvent) => setNarrow(e.matches);
    setNarrow(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return narrow;
}

/** Compact age for the phone HUD: `3h`, `5d`, `now`. */
function shortAge(t: number | null): string {
  if (!t) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 90) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 172_800) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

export function Hud() {
  const lod = useAppStore((s) => s.lod);
  const inView = useAppStore((s) => s.inView);
  const sync = useAppStore((s) => s.sync);
  const zoom = useAppStore((s) => s.zoom);
  const hex = useAppStore((s) => s.hexHover);
  const narrow = useNarrow();
  if (!sync && !lod) return null;
  const filtered = inView && inView.totalByKind.flock + inView.totalByKind.alpr + inView.totalByKind.user > inView.visible;
  const failed = sync && sync.last_outcome && sync.last_outcome !== "ok";
  return (
    <div className="hud" aria-label="Camera data readout">
      <span className="k">IN VIEW</span>
      <span className="v">{inView ? formatExact(inView.visible) : "—"}</span>
      {filtered && inView && (
        <span className="muted">
          of {formatExact(inView.totalByKind.flock + inView.totalByKind.alpr + inView.totalByKind.user)}
        </span>
      )}
      <span className="sep" />
      <span className="k">TOTAL</span>
      <span className="v">{sync ? formatExact(sync.cameras) : "—"}</span>
      <span className="sep" />
      <span className="k">SYNC</span>
      <span className={`v ${failed ? "warn" : ""}`} title={sync?.last_error ?? `Last successful sync: ${syncStamp(sync?.last_ok_at ?? null)}`}>
        {sync?.running
          ? narrow
            ? "running"
            : `running · ${sync.phase ?? ""}`
          : narrow
            ? shortAge(sync?.last_ok_at ?? null)
            : syncStamp(sync?.last_ok_at ?? null)}
        {failed && !sync?.running ? (narrow ? " !" : " · last attempt failed") : ""}
      </span>
      <span className="sep" />
      <span className="k">Z</span>
      <span className="v">{zoom.toFixed(1)}</span>
      {lod && !narrow && <span className="muted">{BAND_NAME[lod.band]}</span>}
      {hex && (
        <>
          <span className="sep" />
          <span className="k">CELL</span>
          <span className="v">{formatExact(hex.count)}</span>
        </>
      )}
    </div>
  );
}

/** 1, 2 or 5 × a power of ten, at most v. */
function niceDown(v: number): number {
  if (v < 1) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [5, 2, 1]) if (m * p <= v) return m * p;
  return p;
}

const LEGEND_OPEN_KEY = "ff.legendOpen";

export function Legend() {
  const lod = useAppStore((s) => s.lod);
  // The bar shows the colours the map uses, which a light theme runs pale to dark.
  const overlay = useMapStyle()?.overlay ?? DARK_OVERLAY;
  const gradient = rampGradientCss(overlay.ramp);
  const shade = overlay.scheme === "light" ? "Darkness" : "Brightness";
  const narrow = useNarrow();
  // On a phone the legend costs real map, so it folds to its title bar; the choice sticks.
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(LEGEND_OPEN_KEY) !== "0";
    } catch {
      return true;
    }
  });
  const toggle = () => {
    setOpen((v) => {
      try {
        localStorage.setItem(LEGEND_OPEN_KEY, v ? "0" : "1");
      } catch {
        /* private mode: this session only */
      }
      return !v;
    });
  };
  if (!lod || !bandHasLegend(lod.band) || lod.included === 0) return null;
  const title = (text: string) =>
    narrow ? (
      <button className="legend-title legend-head" onClick={toggle} aria-expanded={open}>
        <span>{text}</span>
        <span aria-hidden>{open ? "▾" : "▸"}</span>
      </button>
    ) : (
      <div className="legend-title">{text}</div>
    );
  const folded = narrow && !open;

  if (lod.band === "wide") {
    const max = Math.max(1, lod.hexMax);
    const ticks = [1, 10, 100, 1000, 10_000].filter((v) => v <= max);
    const pos = (v: number) => (max <= 1 ? 0 : (Math.log(v) / Math.log(max)) * 100);
    return (
      <div className="legend" aria-label="Legend">
        {title("Cameras per cell")}
        {folded ? null : (
          <>
        <div className="ramp" style={{ background: gradient }} />
        <div className="ticks">
          {ticks.map((v) => (
            <span key={v} style={{ left: `${pos(v)}%` }}>
              {formatCount(v)}
            </span>
          ))}
          <span className="max" style={{ left: "100%" }}>
            {formatCount(max)}
          </span>
        </div>
        <div className="legend-note">
          Log scale · cell ≈ {Math.round(lod.hexWidthKm)} km
          {lod.usersIncluded > 0 && (narrow ? ` · +${lod.usersIncluded} unverified` : "")}
        </div>
        {lod.usersIncluded > 0 && !narrow && (
          <div className="legend-note">Includes {lod.usersIncluded} unverified submission(s)</div>
        )}
          </>
        )}
      </div>
    );
  }

  // Mid band: node area is proportional to count at this level's scale.
  // Reference circles are drawn at their true on-screen size, so they can't be scaled down
  // to fit a phone; show fewer of them instead.
  const steps = narrow ? [1, 16] : [1, 4, 16];
  const refs = Array.from(new Set(steps.map((d) => niceDown(lod.maxCount / d))))
    .filter((c) => c >= lod.minProportional)
    .sort((a, b) => b - a);
  const rMax = refs.length ? nodeRadius(refs[0], lod.scale) : 10;
  const h = rMax * 2 + 4;
  let x = 2;
  const circles = refs.map((c) => {
    const r = nodeRadius(c, lod.scale);
    const cx = x + r;
    x += 2 * r + 12;
    return { c, r, cx };
  });
  return (
    <div className="legend" aria-label="Legend">
      {title("Cluster area = camera count")}
      {folded ? null : (
        <>
      <svg width={Math.max(60, x)} height={h + 14} className="sizes" aria-hidden>
        {circles.map(({ c, r, cx }) => (
          <g key={c}>
            <circle cx={cx} cy={h - r - 2} r={r} fill="none" stroke={rampCss(0.8)} strokeWidth={1} />
            <text x={cx} y={h + 11} textAnchor="middle">
              {formatCount(c)}
            </text>
          </g>
        ))}
      </svg>
      <div className="ramp" style={{ background: gradient }} />
      <div className="ticks">
        <span style={{ left: "0%" }}>sparse</span>
        <span className="max" style={{ left: "100%" }}>
          dense
        </span>
      </div>
      <div className="legend-note">
        {narrow
          ? `Log density · min size < ${formatExact(lod.minProportional)}`
          : `${shade} = log density · below ${formatExact(lod.minProportional)}: minimum size`}
        {lod.usersIncluded > 0 && narrow && (
          <>
            {" · "}
            <span style={{ color: STATE_CSS }}>+n</span> unverified
          </>
        )}
      </div>
      {lod.usersIncluded > 0 && !narrow && (
        <div className="legend-note">
          <span style={{ color: STATE_CSS }}>○ +n</span> unverified submissions (counted in size)
        </div>
      )}
        </>
      )}
    </div>
  );
}

/**
 * What an empty viewport means. Three states that must never be confused: the data has
 * never been synced, the latest sync failed, or there simply are no mapped cameras here.
 */
export function EmptyState() {
  const sync = useAppStore((s) => s.sync);
  const dataset = useAppStore((s) => s.dataset);
  const inView = useAppStore((s) => s.inView);
  const lod = useAppStore((s) => s.lod);
  const dispatch = useAppStore((s) => s.dispatchFilter);
  if (!sync || !dataset) return null;
  const empty = !inView || inView.visible === 0;
  if (!empty || (lod?.building && inView === null)) return null;
  const hidden = inView ? inView.totalByKind.flock + inView.totalByKind.alpr + inView.totalByKind.user - inView.visible : 0;

  if (sync.last_ok_at === null) {
    if (sync.running) {
      return (
        <div className="empty-state" role="status">
          <div className="title">Syncing camera data…</div>
          <div className="sub">
            {sync.phase === "downloading"
              ? `Downloading every mapped ALPR camera from OpenStreetMap · ${mb(sync.bytes)}`
              : sync.phase === "parsing"
                ? "Reading the download"
                : "Saving to this device"}
          </div>
          <div className="sub muted">One request for the whole world. It can take a few minutes; the map fills in when it finishes.</div>
        </div>
      );
    }
    if (sync.last_outcome === "error" || sync.last_outcome === "offline") {
      return (
        <div className="empty-state failed" role="alert">
          <div className="title">Camera sync failed</div>
          <div className="sub">{sync.last_error ?? "Unknown error"}</div>
          <div className="sub muted">
            {sync.last_outcome === "offline" ? "You appear to be offline. " : ""}
            {sync.next_due_at * 1000 > Date.now()
              ? `Retrying automatically at ${new Date(sync.next_due_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}.`
              : "Retrying automatically shortly."}
          </div>
          <button className="btn small" onClick={() => void runSync()}>
            Retry now
          </button>
        </div>
      );
    }
    return (
      <div className="empty-state" role="status">
        <div className="title">Camera data not synced yet</div>
        <div className="sub">This device has no camera data for this area. The first sync downloads every mapped camera once.</div>
        <button className="btn small primary" onClick={() => void runSync()}>
          Sync now
        </button>
      </div>
    );
  }

  if (hidden > 0) {
    return (
      <div className="empty-state" role="status">
        <div className="title">No cameras match the filters here</div>
        <div className="sub">{formatExact(hidden)} camera(s) in view are hidden by the current filters.</div>
        <button className="btn small" onClick={() => dispatch({ type: "reset" })}>
          Reset filters
        </button>
      </div>
    );
  }
  return (
    <div className="empty-state quiet" role="status">
      <div className="title">No cameras mapped here</div>
      <div className="sub">
        Coverage is crowdsourced and incomplete: this means nobody has mapped a camera in this area, not that there are none.
        Data synced {syncStamp(sync.last_ok_at)}
        {sync.last_outcome && sync.last_outcome !== "ok" ? " (the latest sync attempt failed)" : ""}.
      </div>
    </div>
  );
}
