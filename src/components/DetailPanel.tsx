import { copyText, openOsm, reloadSubmissions, toastError } from "../lib/actions";
import { CATEGORY_LABELS, MARKER_COLORS, cameraKind, vendorLabel } from "../lib/classify";
import { formatDirection } from "../lib/direction";
import { compassLabel, formatAgo, formatCoords, formatTime, osmElementUrl } from "../lib/geo";
import { api } from "../lib/ipc";
import type { Camera, Submission, WifiSighting } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import { useState } from "react";
import { useSheet } from "./useSheet";

function WifiDetail({ sighting }: { sighting: WifiSighting }) {
  const setPendingWatchArea = useAppStore((s) => s.setPendingWatchArea);
  const setMode = useAppStore((s) => s.setMode);
  const setDraftPin = useAppStore((s) => s.setDraftPin);
  const select = useAppStore((s) => s.select);
  const place = [sighting.road, sighting.city, sighting.region, sighting.country].filter(Boolean).join(", ");
  return (
    <>
      <div className="section">
        <div className="row">
          <span className="swatch" style={{ background: MARKER_COLORS.wifi, color: MARKER_COLORS.wifi }} />
          <strong>{CATEGORY_LABELS.wifi}</strong>
        </div>
        <div className="callout warn" style={{ marginTop: 8 }}>
          <strong>Suspected, unconfirmed.</strong> This point is a Wi-Fi radio whose MAC prefix (OUI) matches
          hardware associated with Flock Safety. An OUI match is a heuristic: the prefix can belong to unrelated
          devices, the sighting may be stale, and the position is where a passing scanner heard it, not
          necessarily where the device is mounted. It is not an OSM camera and does not feed alerts.
        </div>
        <div className="muted small">
          Source:{" "}
          {sighting.source === "upstream"
            ? "Flock Finder dataset (WiGLE-derived, simeononsecurity/flock-finder)"
            : "your imported Wigle CSV"}
        </div>
      </div>

      <div className="section">
        <h4>Location</h4>
        <div className="row between">
          <code>{formatCoords(sighting.lat, sighting.lon)}</code>
          <button className="btn small" onClick={() => void copyText(formatCoords(sighting.lat, sighting.lon))}>
            Copy
          </button>
        </div>
        {place && <div className="muted small" style={{ marginTop: 6 }}>{place}{sighting.postalcode ? ` ${sighting.postalcode}` : ""}</div>}
        <div className="row wrap" style={{ marginTop: 8 }}>
          <button
            className="btn small"
            onClick={() => {
              select(null);
              setMode("add");
              setDraftPin({ lat: sighting.lat, lon: sighting.lon });
            }}
            title="Only if you have personally observed a camera here"
          >
            I observed a camera here…
          </button>
          <button className="btn small" onClick={() => setPendingWatchArea({ lat: sighting.lat, lon: sighting.lon })}>
            Watch this area
          </button>
        </div>
      </div>

      <div className="section">
        <h4>Radio</h4>
        <div className="kv">
          <div className="k">BSSID</div>
          <div className="v"><code>{sighting.netid}</code></div>
          <div className="k">OUI match</div>
          <div className="v"><code>{sighting.oui}</code></div>
          <div className="k">SSID</div>
          <div className="v">{sighting.ssid ?? <span className="muted">hidden / none</span>}</div>
          <div className="k">Channel</div>
          <div className="v">{sighting.channel ?? "—"}</div>
          <div className="k">Encryption</div>
          <div className="v">{sighting.encryption ?? "—"}</div>
          <div className="k">First heard</div>
          <div className="v">{sighting.first_seen ?? "—"}</div>
          <div className="k">Last heard</div>
          <div className="v">{sighting.last_seen ?? "—"}</div>
          <div className="k">Stored locally</div>
          <div className="v">{formatTime(sighting.imported_at)}</div>
        </div>
      </div>
    </>
  );
}

function CameraDetail({ camera }: { camera: Camera }) {
  const displayTags = useAppStore((s) => s.info?.display_tags ?? []);
  const setPendingWatchArea = useAppStore((s) => s.setPendingWatchArea);
  const kind = cameraKind(camera);
  const vendor = vendorLabel(camera.tags);
  const shown = new Set<string>();
  const rows: [string, string][] = [];
  for (const k of displayTags) {
    if (camera.tags[k] !== undefined) {
      rows.push([k, camera.tags[k]]);
      shown.add(k);
    }
  }
  for (const k of Object.keys(camera.tags).sort()) {
    if (!shown.has(k)) rows.push([k, camera.tags[k]]);
  }
  const url = osmElementUrl(camera.osm_type, camera.osm_id);

  return (
    <>
      <div className="section">
        <div className="row">
          <span className={`swatch ${camera.stale_since ? "hollow" : ""}`} style={{ background: MARKER_COLORS[kind], color: MARKER_COLORS[kind] }} />
          <strong>{CATEGORY_LABELS[kind]}</strong>
        </div>
        {vendor && <div className="muted small">Vendor tag: {vendor}</div>}
        {camera.category === "unknown" && (
          <div className="muted small">Matched the query but carries neither an ALPR tag nor a Flock tag.</div>
        )}
        {camera.stale_since && (
          <div className="callout warn" style={{ marginTop: 8 }}>
            Not present in the latest fetch of this area (since {formatTime(camera.stale_since)}). It may
            have been removed from OSM. Kept for 30 days, shown hollow.
          </div>
        )}
      </div>

      <div className="section">
        <h4>Location</h4>
        <div className="row between">
          <code>{formatCoords(camera.lat, camera.lon)}</code>
          <button className="btn small" onClick={() => void copyText(formatCoords(camera.lat, camera.lon))}>
            Copy
          </button>
        </div>
        <div className="row" style={{ marginTop: 8 }}>
          <button className="btn small" onClick={() => void openOsm(url)}>
            Open on OpenStreetMap ↗
          </button>
          <button
            className="btn small"
            onClick={() => setPendingWatchArea({ lat: camera.lat, lon: camera.lon })}
            title="Create a watch area centred on this camera"
          >
            Watch this area
          </button>
        </div>
        <div className="muted small" style={{ marginTop: 6, wordBreak: "break-all" }}>{url}</div>
      </div>

      <div className="section">
        <h4>Tags ({rows.length})</h4>
        {rows.length === 0 ? (
          <div className="muted small">No tags.</div>
        ) : (
          <div className="kv">
            {rows.map(([k, v]) => (
              <div key={k} style={{ display: "contents" }}>
                <div className="k">{k}</div>
                <div className="v">{k === "direction" || k === "camera:direction" ? formatDirection(v) : v}</div>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="section">
        <h4>Data provenance</h4>
        <div className="kv">
          <div className="k">OSM element</div>
          <div className="v">
            {camera.osm_type} {camera.osm_id}
          </div>
          <div className="k">Last seen in data</div>
          <div className="v">
            {formatTime(camera.last_seen)} ({formatAgo(camera.last_seen)})
          </div>
          <div className="k">First cached</div>
          <div className="v">{formatTime(camera.first_seen)}</div>
        </div>
      </div>
    </>
  );
}

function SubmissionDetail({ submission }: { submission: Submission }) {
  const setSubmissionDraft = useAppStore((s) => s.setSubmissionDraft);
  const setUploadTarget = useAppStore((s) => s.setUploadTarget);
  const select = useAppStore((s) => s.select);
  const pushToast = useAppStore((s) => s.pushToast);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const uploaded = submission.status === "uploaded";

  const doDelete = async () => {
    try {
      await api.deleteSubmission(submission.id);
      pushToast(uploaded ? "Local copy deleted. The OSM element was not touched." : "Submission deleted.", "success");
      select(null);
      await reloadSubmissions();
    } catch (e) {
      toastError(e, "Delete failed");
    }
  };

  const exportOne = async () => {
    try {
      const path = await api.exportJosm([submission.id]);
      if (path) pushToast(`Exported to ${path}`, "success");
    } catch (e) {
      toastError(e, "Export failed");
    }
  };

  const catLabel = { flock: "Flock (user-observed)", alpr: "ALPR (user-observed)", unsure: "ALPR, vendor unsure (user-observed)" }[submission.category];

  return (
    <>
      <div className="section">
        <div className="row">
          <span className="swatch" style={{ background: MARKER_COLORS.user }} />
          <strong>{CATEGORY_LABELS.user}</strong>
        </div>
        <div className="muted small">{catLabel}</div>
        <div className="muted small">
          Status: {uploaded ? `uploaded to OSM (node ${submission.osm_element_id})` : "local only (not yet in OSM)"}
        </div>
      </div>
      <div className="section">
        <h4>Details</h4>
        <div className="kv">
          <div className="k">Coordinates</div>
          <div className="v">
            <code>{formatCoords(submission.lat, submission.lon)}</code>{" "}
            <button className="btn small" onClick={() => void copyText(formatCoords(submission.lat, submission.lon))}>
              Copy
            </button>
          </div>
          <div className="k">Faces</div>
          <div className="v">{submission.direction === null ? "—" : compassLabel(submission.direction)}</div>
          <div className="k">Mount</div>
          <div className="v">{submission.mount ?? "—"}</div>
          <div className="k">Operator</div>
          <div className="v">{submission.operator ?? "—"}</div>
          <div className="k">Private notes</div>
          <div className="v">{submission.notes ?? "—"}</div>
          <div className="k">Created</div>
          <div className="v">{formatTime(submission.created_at)}</div>
        </div>
      </div>
      <div className="section row wrap">
        {!uploaded && (
          <>
            <button
              className="btn small"
              onClick={() => setSubmissionDraft({ id: submission.id, lat: submission.lat, lon: submission.lon, existing: submission })}
            >
              Edit
            </button>
            <button className="btn small" onClick={() => void exportOne()}>
              Export .osm (JOSM)
            </button>
            <button className="btn small primary" onClick={() => setUploadTarget(submission)}>
              Upload to OSM…
            </button>
          </>
        )}
        {uploaded && submission.osm_element_id !== null && (
          <button className="btn small" onClick={() => void openOsm(osmElementUrl("node", submission.osm_element_id!))}>
            Open on OpenStreetMap ↗
          </button>
        )}
        {!confirmDelete ? (
          <button className="btn small danger" onClick={() => setConfirmDelete(true)}>
            Delete
          </button>
        ) : (
          <div className="callout warn" style={{ width: "100%" }}>
            {uploaded
              ? "This deletes only the local copy. The camera stays on OpenStreetMap; edit or delete it there if it is wrong."
              : "Delete this submission? This cannot be undone."}
            <div className="row" style={{ marginTop: 8 }}>
              <button className="btn small danger" onClick={() => void doDelete()}>
                Confirm delete
              </button>
              <button className="btn small" onClick={() => setConfirmDelete(false)}>
                Keep
              </button>
            </div>
          </div>
        )}
      </div>
    </>
  );
}

export default function DetailPanel() {
  const selection = useAppStore((s) => s.selection);
  // A new selection opens the sheet again.
  const sheet = useSheet(selection);
  const select = useAppStore((s) => s.select);
  if (!selection) return null;
  return (
    <div ref={sheet.ref} className={`panel right ${sheet.className}`}>
      <div className="panel-header" {...sheet.headerProps}>
        {sheet.grip}
        <span>
          {selection.kind === "camera" ? "Camera" : selection.kind === "submission" ? "Your submission" : "Wi-Fi sighting"}
        </span>
        <button className="close" onClick={() => select(null)} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        {selection.kind === "camera" ? (
          <CameraDetail camera={selection.camera} />
        ) : selection.kind === "submission" ? (
          <SubmissionDetail submission={selection.submission} />
        ) : (
          <WifiDetail sighting={selection.sighting} />
        )}
      </div>
    </div>
  );
}
