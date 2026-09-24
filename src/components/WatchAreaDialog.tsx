import { useState } from "react";
import { reloadAlertState, toastError } from "../lib/actions";
import { formatCoords, formatDistance } from "../lib/geo";
import { api } from "../lib/ipc";
import { useAppStore } from "../store/useAppStore";
import Modal from "./Modal";

export default function WatchAreaDialog() {
  const pending = useAppStore((s) => s.pendingWatchArea);
  const setPending = useAppStore((s) => s.setPendingWatchArea);
  const pushToast = useAppStore((s) => s.pushToast);
  const setPanel = useAppStore((s) => s.setPanel);
  const [name, setName] = useState("");
  const [radius, setRadius] = useState(1000);
  const [busy, setBusy] = useState(false);
  if (!pending) return null;

  const close = () => setPending(null);
  const create = async () => {
    setBusy(true);
    try {
      const r = await api.createWatchArea(name.trim() || "Watch area", pending.lat, pending.lon, radius);
      if (r.offline || r.baseline_pending) {
        pushToast(`Watch area “${r.target.name}” saved. Offline, so its baseline will be recorded silently at the next successful check.`, "warn");
      } else {
        pushToast(`Watching “${r.target.name}”: ${r.count} camera(s) currently known inside. No alert for those; you will be told about new ones.`, "success");
      }
      await reloadAlertState();
      close();
      setPanel("alerts");
    } catch (e) {
      toastError(e, "Could not create watch area");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="New watch area"
      onClose={close}
      footer={
        <>
          <button className="btn" onClick={close} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={() => void create()} disabled={busy}>
            {busy ? "Checking area…" : "Create"}
          </button>
        </>
      }
    >
      <div className="muted small" style={{ marginBottom: 10 }}>
        Centre: <code>{formatCoords(pending.lat, pending.lon)}</code>. The area is checked now (from cache
        where fresh), and again on each background refresh. Newly mapped cameras trigger a notification;
        the initial set does not.
      </div>
      <div className="field">
        <label>Name</label>
        <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Home, Work, School…" autoFocus />
      </div>
      <div className="field">
        <label>Radius: {formatDistance(radius)} (100 m – 10 km)</label>
        <input type="range" min={100} max={10000} step={50} value={radius} onChange={(e) => setRadius(Number(e.target.value))} />
        <input className="input" type="number" min={100} max={10000} value={radius} onChange={(e) => setRadius(Math.min(10000, Math.max(100, Number(e.target.value) || 100)))} style={{ width: 140 }} />
      </div>
    </Modal>
  );
}
