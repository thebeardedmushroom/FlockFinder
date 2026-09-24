import { useEffect, useState } from "react";
import { reloadSubmissions } from "../lib/actions";
import { formatCoords } from "../lib/geo";
import { api } from "../lib/ipc";
import { errorMessage, isAppError, type OsmAuthStatus } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import Modal from "./Modal";

/**
 * Mandatory preflight before an OSM upload. There is no form element and no keyboard
 * shortcut: the only way to upload is the button, which stays disabled until the
 * observation checkbox is ticked and a comment is present. The backend refuses
 * uploads without `confirmed: true` as well.
 */
export default function OsmUploadDialog() {
  const target = useAppStore((s) => s.uploadTarget);
  const setTarget = useAppStore((s) => s.setUploadTarget);
  const setPanel = useAppStore((s) => s.setPanel);
  const pushToast = useAppStore((s) => s.pushToast);
  const [tags, setTags] = useState<[string, string][] | null>(null);
  const [auth, setAuth] = useState<OsmAuthStatus | null>(null);
  const [comment, setComment] = useState("");
  const [observed, setObserved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ kind: string; message: string } | null>(null);

  useEffect(() => {
    if (!target) return;
    setTags(null);
    setError(null);
    setObserved(false);
    setComment(`Add ${target.category === "flock" ? "Flock Safety" : "ALPR"} camera (surveyed)`);
    void api.previewSubmissionTags(target.id).then(setTags).catch((e) => setError({ kind: "other", message: errorMessage(e) }));
    void api.osmAuthStatus().then(setAuth).catch(() => setAuth(null));
  }, [target]);

  if (!target) return null;
  const close = () => setTarget(null);
  const canUpload = observed && comment.trim() !== "" && !busy && tags !== null && !!auth?.signed_in;

  const upload = async () => {
    if (!canUpload) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.osmUploadSubmission(target.id, comment.trim(), observed);
      pushToast(`Uploaded as OSM node ${r.node_id} in changeset ${r.changeset_id}.`, "success");
      await reloadSubmissions();
      close();
    } catch (e) {
      // Shown verbatim; never retried automatically.
      setError(isAppError(e) ? { kind: e.kind, message: e.message } : { kind: "other", message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Upload to OpenStreetMap — review before sending"
      onClose={busy ? undefined : close}
      footer={
        <>
          <button className="btn" onClick={close} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" onClick={() => void upload()} disabled={!canUpload}>
            {busy ? "Uploading…" : "Upload one changeset"}
          </button>
        </>
      }
    >
      {auth && !auth.signed_in && (
        <div className="callout warn">
          {auth.configured ? "You are not signed in to OpenStreetMap." : "OSM upload is not configured."}{" "}
          <button className="btn small" onClick={() => { close(); setPanel("settings"); }}>
            Open Settings
          </button>{" "}
          Or export this submission as .osm and upload it with JOSM instead.
        </div>
      )}
      {error && (
        <div className="callout error">
          {error.kind === "auth_required" ? (
            <>
              Your OSM session is no longer valid. Sign in again from Settings, then retry.{" "}
              <button className="btn small" onClick={() => { close(); setPanel("settings"); }}>
                Open Settings
              </button>
            </>
          ) : (
            <>
              <strong>Upload failed.</strong> The submission is still local. OSM said:
              <pre className="code" style={{ marginTop: 6 }}>{error.message}</pre>
            </>
          )}
        </div>
      )}

      <div className="section">
        <h4>Exactly these tags will be written</h4>
        <div className="muted small" style={{ marginBottom: 6 }}>
          Node at <code>{formatCoords(target.lat, target.lon)}</code>. One camera per changeset. Private notes are not included.
        </div>
        {tags === null ? (
          <div className="muted">Loading…</div>
        ) : (
          <pre className="code">{tags.map(([k, v]) => `${k}=${v}`).join("\n")}</pre>
        )}
      </div>

      <div className="field">
        <label>Changeset comment (required)</label>
        <input
          className="input"
          value={comment}
          onChange={(e) => setComment(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.preventDefault();
          }}
        />
        <span className="muted small">
          The changeset will also carry <code>created_by=FlockFinder/{useAppStore.getState().info?.version ?? "?"}</code>.
        </span>
      </div>

      <label className="checkbox" style={{ alignItems: "flex-start" }}>
        <input type="checkbox" checked={observed} onChange={(e) => setObserved(e.target.checked)} style={{ marginTop: 3 }} />
        <span>
          I have <strong>personally observed</strong> this camera at this location. I am not copying it from
          another map, a news article, a vendor site, or a guess.
        </span>
      </label>
    </Modal>
  );
}
