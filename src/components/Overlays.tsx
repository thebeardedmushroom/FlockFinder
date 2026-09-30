import { useEffect } from "react";
import { copyText } from "../lib/actions";
import { coordLabel } from "../lib/directions";
import { formatCoords } from "../lib/geo";
import { api } from "../lib/ipc";
import { useAppStore } from "../store/useAppStore";

export function StatusChips() {
  const offline = useAppStore((s) => s.offline);
  const fixture = useAppStore((s) => s.info?.fixture_mode);
  const sync = useAppStore((s) => s.sync);
  const building = useAppStore((s) => s.lod?.building && s.dataset !== null);
  const wifiInView = useAppStore((s) => Object.keys(s.sightings).length);
  return (
    <div className="chips">
      {sync?.running && (
        <span className="chip loading">
          Syncing cameras · {sync.phase}
          {sync.phase === "downloading" && sync.bytes > 0 ? ` · ${(sync.bytes / 1_048_576).toFixed(1)} MB` : ""}
        </span>
      )}
      {building && <span className="chip loading">Indexing…</span>}
      {wifiInView > 0 && <span className="chip">{wifiInView} Wi-Fi sightings in view</span>}
      {offline && <span className="chip offline">⚠ Offline — showing the cameras on this device</span>}
      {fixture && <span className="chip">Fixture mode: no Overpass requests</span>}
    </div>
  );
}

export function Footer() {
  const disclaimer = useAppStore(
    (s) =>
      s.info?.disclaimer ??
      "Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence of a camera.",
  );
  return (
    <div className="footer">
      <span>{disclaimer}</span>
      <span className="attribution">© OpenStreetMap contributors (ODbL)</span>
    </div>
  );
}

export function Toasts() {
  const toasts = useAppStore((s) => s.toasts);
  const dismiss = useAppStore((s) => s.dismissToast);
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.tone}`}>
          <span className="text">{t.text}</span>
          {t.action && (
            <button
              className="btn small"
              onClick={() => {
                t.action?.run();
                dismiss(t.id);
              }}
            >
              {t.action.label}
            </button>
          )}
          <button className="close" onClick={() => dismiss(t.id)} aria-label="Dismiss">
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

export function ContextMenu() {
  const menu = useAppStore((s) => s.contextMenu);
  const setMenu = useAppStore((s) => s.setContextMenu);
  const setPendingWatchArea = useAppStore((s) => s.setPendingWatchArea);
  const setMode = useAppStore((s) => s.setMode);
  const setDraftPin = useAppStore((s) => s.setDraftPin);
  const setEndpoint = useAppStore((s) => s.setEndpoint);
  const setPanel = useAppStore((s) => s.setPanel);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [menu, setMenu]);

  if (!menu) return null;
  // Positioned inside the map container; flip so the menu never runs off-screen.
  const MENU_W = 220;
  const MENU_H = 240;
  const left = Math.max(4, Math.min(menu.x + 4, window.innerWidth - MENU_W - 4));
  const top = menu.y + MENU_H + 40 > window.innerHeight ? Math.max(4, menu.y - MENU_H) : menu.y + 4;
  return (
    <div className="context-menu" style={{ left, top }}>
      <div className="coords">{formatCoords(menu.lat, menu.lon)}</div>
      {(["start", "end"] as const).map((which) => (
        <button
          key={which}
          onClick={() => {
            setEndpoint(which, { lat: menu.lat, lon: menu.lon, label: coordLabel(menu.lat, menu.lon) });
            setPanel("directions");
          }}
        >
          {which === "start" ? "Directions from here" : "Directions to here"}
        </button>
      ))}
      <button onClick={() => setPendingWatchArea({ lat: menu.lat, lon: menu.lon })}>
        Create watch area here
      </button>
      <button
        onClick={() => {
          setMode("add");
          setDraftPin({ lat: menu.lat, lon: menu.lon });
          setMenu(null);
        }}
      >
        Add camera here
      </button>
      <button
        onClick={() => {
          void copyText(formatCoords(menu.lat, menu.lon));
          setMenu(null);
        }}
      >
        Copy coordinates
      </button>
      <button onClick={() => setMenu(null)}>Cancel</button>
    </div>
  );
}

export function FirstRunDialog() {
  const open = useAppStore((s) => s.firstRunOpen);
  const setOpen = useAppStore((s) => s.setFirstRunOpen);
  const info = useAppStore((s) => s.info);
  if (!open) return null;
  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-label="Before you start">
        <div className="panel-header">Before you start</div>
        <div className="panel-body">
          <div className="callout honesty">
            {info?.disclaimer ??
              "Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence of a camera."}
          </div>
          <p>
            Flock Finder shows automated license plate reader (ALPR) cameras that volunteers have
            added to OpenStreetMap. Mapping is patchy: some cities are well covered, most are not.
            An empty map means nobody has mapped that area yet, not that it has no cameras.
          </p>
          <p>
            The app maps fixed hardware only. It never handles license plate data, vehicle data,
            or information about people. It uses your location only while the locate button is
            on, to show where you are and, unless you turn proximity alerts off, to warn you when
            you come near a mapped camera. Your position is never stored or uploaded, and it is
            checked against cameras already on this device: the app downloads every mapped
            camera in one periodic request, so moving the map or yourself tells no server where
            you are looking. Watch-area alerts are based on places and routes you save yourself.
          </p>
          <p className="muted small">
            Data © OpenStreetMap contributors, available under the Open Database License (ODbL).
            Overpass API and Nominatim are shared community services; the app rate-limits and
            caches its requests to be a good citizen.
          </p>
        </div>
        <div className="modal-footer">
          <button
            className="btn primary"
            onClick={async () => {
              try {
                await api.markFirstRunDone();
              } catch {
                /* non-fatal */
              }
              setOpen(false);
            }}
          >
            I understand
          </button>
        </div>
      </div>
    </div>
  );
}
