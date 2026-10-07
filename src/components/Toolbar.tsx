import { useEffect, useRef, useState } from "react";
import { runSync, toastError } from "../lib/actions";
import { activeFilterCount } from "../lib/filters";
import { formatAgo, formatDistance } from "../lib/geo";
import { api } from "../lib/ipc";
import type { GeocodeResult } from "../lib/types";
import { useAppStore } from "../store/useAppStore";

function SearchBox() {
  const [q, setQ] = useState("");
  const [results, setResults] = useState<GeocodeResult[] | null>(null);
  const [busy, setBusy] = useState(false);
  const flyTo = useAppStore((s) => s.flyTo);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setResults(null);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, []);

  const run = async () => {
    const query = q.trim();
    if (!query || busy) return;
    setBusy(true);
    try {
      const r = await api.geocode(query);
      setResults(r);
      if (r.length === 0) useAppStore.getState().pushToast("No places found for that search.", "info");
    } catch (e) {
      toastError(e, "Search failed");
    } finally {
      setBusy(false);
    }
  };

  const choose = (r: GeocodeResult) => {
    setResults(null);
    if (r.bbox) flyTo({ bbox: r.bbox });
    else flyTo({ lat: r.lat, lon: r.lon, zoom: 14 });
    // The place's detail (pinned on the map) can save it or route to it.
    const name = r.display_name.split(",")[0]?.trim() || null;
    useAppStore.getState().select({ kind: "place", place: { lat: r.lat, lon: r.lon, name, address: r.display_name, source: "search" } });
  };

  return (
    <div className="search" ref={boxRef}>
      <input
        placeholder={busy ? "Searching…" : "Search a place (Nominatim)…"}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void run();
          if (e.key === "Escape") setResults(null);
        }}
        aria-label="Search a place"
      />
      {results && results.length > 0 && (
        <div className="results">
          {results.map((r, i) => (
            <button key={i} onClick={() => choose(r)}>
              {r.display_name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function Toolbar() {
  const panel = useAppStore((s) => s.panel);
  const togglePanel = useAppStore((s) => s.togglePanel);
  const mode = useAppStore((s) => s.mode);
  const setMode = useAppStore((s) => s.setMode);
  const sync = useAppStore((s) => s.sync);
  const filters = useAppStore((s) => s.filters);
  const unseen = useAppStore((s) => s.alertState?.unseen_added ?? 0);
  const localCount = useAppStore((s) => s.submissions.filter((x) => x.status === "local").length);
  const filterCount = activeFilterCount(filters);
  // On narrow screens the buttons collapse into a dropdown behind the menu toggle.
  const [menuOpen, setMenuOpen] = useState(false);
  const pick = (run: () => void) => () => {
    setMenuOpen(false);
    run();
  };
  // A press anywhere outside the dropdown (the map, the search box) closes it.
  const toggleRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!menuRef.current?.contains(t) && !toggleRef.current?.contains(t)) setMenuOpen(false);
    };
    window.addEventListener("pointerdown", onDown);
    return () => window.removeEventListener("pointerdown", onDown);
  }, [menuOpen]);

  return (
    <div className="toolbar">
      <div className="brand">
        <span className="dot" />
        <span className="brand-name">Flock Finder</span>
      </div>
      <SearchBox />
      <button
        ref={toggleRef}
        className={`btn menu-toggle ${menuOpen ? "active" : ""}`}
        onClick={() => setMenuOpen(!menuOpen)}
        aria-label="Menu"
        aria-expanded={menuOpen}
      >
        {menuOpen ? "✕" : "☰"}
        {!menuOpen && unseen > 0 && <span className="badge">{unseen}</span>}
      </button>
      <div ref={menuRef} className={`toolbar-buttons ${menuOpen ? "open" : ""}`}>
        <button
          className={`btn ${panel === "filters" ? "active" : ""}`}
          onClick={pick(() => togglePanel("filters"))}
          title="Filter which markers are shown"
        >
          Filters {filterCount > 0 && <span className="badge">{filterCount}</span>}
        </button>
        <button
          className={`btn ${panel === "directions" ? "active" : ""}`}
          onClick={pick(() => togglePanel("directions"))}
          title="Driving directions that avoid mapped cameras"
        >
          Directions
        </button>
        <button
          className={`btn ${mode === "add" ? "active" : ""}`}
          onClick={pick(() => setMode(mode === "add" ? "view" : "add"))}
          title="Click the map to place a camera you have observed"
        >
          {mode === "add" ? "Cancel add" : "＋ Add camera"}
        </button>
        <button
          className={`btn ${panel === "submissions" ? "active" : ""}`}
          onClick={pick(() => togglePanel("submissions"))}
          title="Your local submissions"
        >
          Submissions {localCount > 0 && <span className="badge neutral">{localCount}</span>}
        </button>
        <button
          className={`btn ${panel === "alerts" ? "active" : ""}`}
          onClick={pick(() => togglePanel("alerts"))}
          title="Watch areas and routes"
        >
          Alerts {unseen > 0 && <span className="badge">{unseen}</span>}
        </button>
        <button
          className="btn"
          disabled={!sync || sync.running}
          onClick={pick(() => void runSync())}
          title={`Download every mapped camera again (one Overpass request). Last sync: ${formatAgo(sync?.last_ok_at)}.`}
        >
          ↻ {sync?.running ? "Syncing…" : "Sync now"}
        </button>
        <button
          className={`btn ${panel === "settings" ? "active" : ""}`}
          onClick={pick(() => togglePanel("settings"))}
        >
          Settings
        </button>
      </div>
    </div>
  );
}

export function ModeBar() {
  const mode = useAppStore((s) => s.mode);
  const setMode = useAppStore((s) => s.setMode);
  const drawPoints = useAppStore((s) => s.drawPoints);
  const undo = useAppStore((s) => s.undoDrawPoint);
  const draftPin = useAppStore((s) => s.draftPin);
  const setSubmissionDraft = useAppStore((s) => s.setSubmissionDraft);
  const setPendingRoute = useAppStore((s) => s.setPendingRoute);
  const picking = useAppStore((s) => s.directions.picking);

  if (mode === "pick") {
    return (
      <div className="mode-bar">
        <span>{picking === "end" ? "Click the map to set the destination." : "Click the map to set the start."}</span>
        <button className="btn small" onClick={() => setMode("view")}>
          Cancel
        </button>
      </div>
    );
  }
  if (mode === "add") {
    return (
      <div className="mode-bar">
        <span>
          {draftPin
            ? "Drag the pin to refine, then continue."
            : "Click the map where the camera is mounted."}
        </span>
        {draftPin && (
          <button
            className="btn small primary"
            onClick={() =>
              setSubmissionDraft({ id: null, lat: draftPin.lat, lon: draftPin.lon, existing: null })
            }
          >
            Continue
          </button>
        )}
        <button className="btn small" onClick={() => setMode("view")}>
          Cancel
        </button>
      </div>
    );
  }
  if (mode === "draw") {
    let length = 0;
    for (let i = 1; i < drawPoints.length; i++) {
      const [a, b] = [drawPoints[i - 1], drawPoints[i]];
      length += Math.hypot((a[0] - b[0]) * 111_320, (a[1] - b[1]) * 111_320 * Math.cos((a[0] * Math.PI) / 180));
    }
    return (
      <div className="mode-bar">
        <span>
          Click to add route points ({drawPoints.length}
          {drawPoints.length >= 2 ? `, ≈ ${formatDistance(length)}` : ""}). Double-click or Finish
          when done.
        </span>
        <button className="btn small" disabled={drawPoints.length === 0} onClick={undo}>
          Undo
        </button>
        <button
          className="btn small primary"
          disabled={drawPoints.length < 2}
          onClick={() => {
            setPendingRoute({ points: drawPoints, name: "", lengthM: length, source: "draw" });
            setMode("view");
          }}
        >
          Finish
        </button>
        <button className="btn small" onClick={() => setMode("view")}>
          Cancel
        </button>
      </div>
    );
  }
  return null;
}
