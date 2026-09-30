import { toastError } from "../lib/actions";
import { formatCoords, formatTime } from "../lib/geo";
import { api } from "../lib/ipc";
import { useAppStore } from "../store/useAppStore";
import { useSheet } from "./useSheet";

export default function SubmissionsPanel() {
  const sheet = useSheet();
  const submissions = useAppStore((s) => s.submissions);
  const setPanel = useAppStore((s) => s.setPanel);
  const setMode = useAppStore((s) => s.setMode);
  const select = useAppStore((s) => s.select);
  const selection = useAppStore((s) => s.selection);
  const flyTo = useAppStore((s) => s.flyTo);
  const pushToast = useAppStore((s) => s.pushToast);
  const setUploadTarget = useAppStore((s) => s.setUploadTarget);
  const local = submissions.filter((s) => s.status === "local");

  const exportAll = async () => {
    try {
      const path = await api.exportJosm([]);
      if (path) pushToast(`Exported ${local.length} submission(s) to ${path}. Open it in JOSM to review and upload.`, "success");
    } catch (e) {
      toastError(e, "Export failed");
    }
  };

  return (
    <div ref={sheet.ref} className={`panel ${sheet.className}`}>
      <div className="panel-header" {...sheet.headerProps}>
        {sheet.grip}
        <span>Your submissions</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        <div className="row wrap" style={{ marginBottom: 12 }}>
          <button className="btn small primary" onClick={() => { setMode("add"); setPanel("none"); }}>
            ＋ Add camera
          </button>
          <button className="btn small" disabled={local.length === 0} onClick={() => void exportAll()} title="JOSM-compatible .osm file with all local submissions">
            Export all local as .osm
          </button>
        </div>
        <div className="muted small" style={{ marginBottom: 12 }}>
          Submissions stay on this machine until you upload them. The JOSM export works without an OSM
          account configured in the app: open the file in JOSM, review, and upload from there.
        </div>
        {submissions.length === 0 && <div className="muted">No submissions yet. Use “Add camera” and click the map.</div>}
        <div className="list">
          {submissions.map((s) => (
            <div key={s.id} className={`list-item ${selection?.kind === "submission" && selection.submission.id === s.id ? "selected" : ""}`}>
              <div className="title">
                <span className="swatch" style={{ background: "var(--user)" }} />
                {s.category === "flock" ? "Flock" : s.category === "alpr" ? "ALPR" : "ALPR (vendor unsure)"}
                <span className={`badge ${s.status === "uploaded" ? "" : "neutral"}`} style={s.status === "uploaded" ? { background: "var(--ok)" } : undefined}>
                  {s.status}
                </span>
              </div>
              <div className="muted small">
                {formatCoords(s.lat, s.lon)}
                {s.operator ? ` · ${s.operator}` : ""} · {formatTime(s.created_at)}
              </div>
              <div className="row wrap" style={{ marginTop: 6 }}>
                <button
                  className="btn small"
                  onClick={() => {
                    select({ kind: "submission", submission: s });
                    flyTo({ lat: s.lat, lon: s.lon, zoom: 17 });
                  }}
                >
                  Show
                </button>
                {s.status === "local" && (
                  <button className="btn small" onClick={() => setUploadTarget(s)}>
                    Upload to OSM…
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
