import { useState } from "react";
import { reloadSubmissions, toastError } from "../lib/actions";
import { COMPASS_POINTS, formatCoords, formatDistance } from "../lib/geo";
import { api } from "../lib/ipc";
import type { Mount, Proximity, SubmissionCategory, SubmissionInput } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import Modal from "./Modal";

const MOUNTS: { value: Mount | ""; label: string }[] = [
  { value: "", label: "Not specified" },
  { value: "pole", label: "Pole" },
  { value: "mast", label: "Mast" },
  { value: "building", label: "Building / wall" },
  { value: "other", label: "Other" },
];

export default function SubmissionForm() {
  const draft = useAppStore((s) => s.submissionDraft);
  const setDraft = useAppStore((s) => s.setSubmissionDraft);
  const setMode = useAppStore((s) => s.setMode);
  const select = useAppStore((s) => s.select);
  const pushToast = useAppStore((s) => s.pushToast);
  const flyTo = useAppStore((s) => s.flyTo);
  const existing = draft?.existing ?? null;

  const [category, setCategory] = useState<SubmissionCategory>(existing?.category ?? "unsure");
  const [direction, setDirection] = useState<string>(existing?.direction?.toString() ?? "");
  const [mount, setMount] = useState<Mount | "">(existing?.mount ?? "");
  const [operator, setOperator] = useState(existing?.operator ?? "");
  const [notes, setNotes] = useState(existing?.notes ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [proximity, setProximity] = useState<Proximity | null>(null);
  const [landWarning, setLandWarning] = useState<string | null>(null);

  if (!draft) return null;

  const close = () => {
    setDraft(null);
    setMode("view");
  };

  const buildInput = (): SubmissionInput | null => {
    const dir = direction.trim() === "" ? null : Number(direction);
    if (dir !== null && (!Number.isInteger(dir) || dir < 0 || dir > 359)) {
      setError("Direction must be a whole number from 0 to 359.");
      return null;
    }
    return {
      lat: draft.lat,
      lon: draft.lon,
      category,
      direction: dir,
      mount: mount === "" ? null : mount,
      operator: operator.trim() === "" ? null : operator.trim(),
      notes: notes.trim() === "" ? null : notes.trim(),
    };
  };

  const persist = async (input: SubmissionInput) => {
    const saved = draft.id === null ? await api.createSubmission(input) : await api.updateSubmission(draft.id, input);
    await reloadSubmissions();
    pushToast(draft.id === null ? "Camera saved locally." : "Submission updated.", "success");
    select({ kind: "submission", submission: saved });
    close();
  };

  const attemptSave = async (skipDuplicateCheck: boolean) => {
    setError(null);
    const input = buildInput();
    if (!input) return;
    setBusy(true);
    try {
      if (!skipDuplicateCheck) {
        const prox = await api.checkSubmissionProximity(input.lat, input.lon, draft.id);
        if (prox.submissions.length > 0 || prox.cameras.length > 0) {
          setProximity(prox);
          return;
        }
      }
      const land = await api.checkLocation(input.lat, input.lon);
      if (land.checked && !land.on_land) {
        setError("This point appears to be in open water. Move the pin onto land before saving.");
        return;
      }
      if (!land.checked) setLandWarning("Could not verify the location is on land (offline). Saved anyway.");
      await persist(input);
    } catch (e) {
      toastError(e, "Could not save");
    } finally {
      setBusy(false);
    }
  };

  const openExisting = () => {
    const cam = proximity?.cameras[0]?.camera;
    if (!cam) return;
    select({ kind: "camera", camera: cam });
    flyTo({ lat: cam.lat, lon: cam.lon, zoom: 18 });
    close();
  };

  const dirNum = direction.trim() === "" ? null : Number(direction);

  return (
    <Modal
      title={draft.id === null ? "Add a camera you have observed" : "Edit submission"}
      onClose={close}
      footer={
        <>
          <button className="btn" onClick={close} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={() => void attemptSave(false)} disabled={busy || proximity !== null}>
            {busy ? "Checking…" : "Save locally"}
          </button>
        </>
      }
    >
      <div className="muted small" style={{ marginBottom: 10 }}>
        Location: <code>{formatCoords(draft.lat, draft.lon)}</code> — saved on this machine only until you
        upload it or export it for JOSM. Notes are private and never leave the app.
      </div>

      {error && <div className="callout error">{error}</div>}
      {landWarning && <div className="callout warn">{landWarning}</div>}

      {proximity && (
        <div className="callout warn">
          {proximity.cameras.length > 0 && (
            <div style={{ marginBottom: 6 }}>
              <strong>Possibly already mapped.</strong> An OSM camera is{" "}
              {formatDistance(proximity.cameras[0].distance_m)} from this pin. If it is the same camera, open it
              instead of adding a duplicate.
            </div>
          )}
          {proximity.submissions.length > 0 && (
            <div style={{ marginBottom: 6 }}>
              <strong>Possible duplicate.</strong> You already have a submission{" "}
              {formatDistance(proximity.submissions[0].distance_m)} away.
            </div>
          )}
          <div className="row wrap">
            {proximity.cameras.length > 0 && (
              <button className="btn small" onClick={openExisting}>
                Open existing camera
              </button>
            )}
            <button className="btn small primary" onClick={() => { setProximity(null); void attemptSave(true); }}>
              Save anyway
            </button>
            <button className="btn small" onClick={() => setProximity(null)}>
              Back
            </button>
          </div>
        </div>
      )}

      <div className="field">
        <label>Category</label>
        <select className="select" value={category} onChange={(e) => setCategory(e.target.value as SubmissionCategory)}>
          <option value="flock">Flock Safety camera</option>
          <option value="alpr">ALPR camera, other or unknown vendor</option>
          <option value="unsure">Not sure of the vendor</option>
        </select>
      </div>

      <div className="field">
        <label>Direction the camera faces (optional)</label>
        <div className="compass">
          {COMPASS_POINTS.map((p) => (
            <button key={p.label} className={dirNum === p.deg ? "active" : ""} onClick={() => setDirection(String(p.deg))} type="button">
              {p.label}
            </button>
          ))}
        </div>
        <div className="row" style={{ marginTop: 6 }}>
          <input className="input" style={{ width: 120 }} placeholder="degrees 0–359" value={direction} onChange={(e) => setDirection(e.target.value)} />
          {direction !== "" && (
            <button className="btn small" type="button" onClick={() => setDirection("")}>
              Remove
            </button>
          )}
        </div>
      </div>

      <div className="field">
        <label>Mount (optional)</label>
        <select className="select" value={mount} onChange={(e) => setMount(e.target.value as Mount | "")}>
          {MOUNTS.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </select>
      </div>

      <div className="field">
        <label>Operator (optional, e.g. a city police department or a retailer)</label>
        <input className="input" value={operator} onChange={(e) => setOperator(e.target.value)} />
      </div>

      <div className="field">
        <label>Private notes (never uploaded)</label>
        <textarea className="textarea" value={notes} onChange={(e) => setNotes(e.target.value)} />
      </div>
    </Modal>
  );
}
