import { useEffect, useState } from "react";
import { reloadAlertState, toastError } from "../lib/actions";
import { bboxOf, formatDistance } from "../lib/geo";
import { api } from "../lib/ipc";
import { useAppStore } from "../store/useAppStore";
import Modal from "./Modal";

export default function RouteDialog() {
  const pending = useAppStore((s) => s.pendingRoute);
  const setPending = useAppStore((s) => s.setPendingRoute);
  const pushToast = useAppStore((s) => s.pushToast);
  const setPanel = useAppStore((s) => s.setPanel);
  const flyTo = useAppStore((s) => s.flyTo);
  const clearDraw = useAppStore((s) => s.clearDraw);
  const [name, setName] = useState("");
  const [corridor, setCorridor] = useState(100);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (pending) {
      setName(pending.name);
      if (pending.source === "gpx") {
        const b = bboxOf(pending.points);
        if (b) flyTo({ bbox: b });
      }
    }
  }, [pending, flyTo]);

  if (!pending) return null;
  const close = () => {
    setPending(null);
    clearDraw();
  };

  const create = async () => {
    setBusy(true);
    try {
      const r = await api.createRoute(name.trim() || "Route", pending.points, corridor);
      if (r.offline || r.baseline_pending) {
        pushToast(`Route “${r.target.name}” saved. Offline, so its baseline will be recorded at the next successful check.`, "warn");
      } else {
        pushToast(`Route “${r.target.name}”: ${r.count} camera(s) currently known within ${corridor} m. Open the Alerts panel for the report.`, "success");
      }
      await reloadAlertState();
      close();
      setPanel("alerts");
    } catch (e) {
      toastError(e, "Could not create route");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={pending.source === "gpx" ? "Import route from GPX" : "Save drawn route"}
      onClose={close}
      footer={
        <>
          <button className="btn" onClick={close} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={() => void create()} disabled={busy}>
            {busy ? "Checking corridor…" : "Save route"}
          </button>
        </>
      }
    >
      <div className="muted small" style={{ marginBottom: 10 }}>
        {pending.points.length} points{pending.lengthM !== null ? `, ≈ ${formatDistance(pending.lengthM)}` : ""}. Only
        the map cells the route actually passes through are queried.
      </div>
      <div className="field">
        <label>Name</label>
        <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Commute, School run…" autoFocus />
      </div>
      <div className="field">
        <label>Corridor width: ±{corridor} m (50 m – 1 km)</label>
        <input type="range" min={50} max={1000} step={10} value={corridor} onChange={(e) => setCorridor(Number(e.target.value))} />
        <input className="input" type="number" min={50} max={1000} value={corridor} onChange={(e) => setCorridor(Math.min(1000, Math.max(50, Number(e.target.value) || 50)))} style={{ width: 140 }} />
      </div>
    </Modal>
  );
}
