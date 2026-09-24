import { useEffect, useState } from "react";
import { reloadAlertState, showCameraKeysOnMap, showCamerasOnMap, toastError } from "../lib/actions";
import { formatAgo, formatDistance, formatTime } from "../lib/geo";
import { api } from "../lib/ipc";
import type { AlertEvent, AreaSummary, RouteReport, RouteSummary } from "../lib/types";
import { useAppStore } from "../store/useAppStore";

function History({ type, id }: { type: "area" | "route"; id: number }) {
  const [events, setEvents] = useState<AlertEvent[] | null>(null);
  useEffect(() => {
    void api.getAlertHistory(type, id).then(setEvents).catch((e) => toastError(e));
  }, [type, id]);
  if (!events) return <div className="muted small">Loading history…</div>;
  if (events.length === 0) return <div className="muted small">No checks recorded yet.</div>;
  return (
    <div className="history">
      {events.map((e) => (
        <div key={e.id} className="ev">
          <span className="muted">{formatTime(e.occurred_at)}</span>
          <span className={e.event}>{e.event}</span>
          <button className="btn small" style={{ height: 20, padding: "0 6px" }} onClick={() => void showCameraKeysOnMap([`${e.osm_type}/${e.osm_id}`])}>
            {e.osm_type}/{e.osm_id}
          </button>
          {e.event === "added" && !e.notified && <span className="muted">(no OS notification)</span>}
        </div>
      ))}
    </div>
  );
}

function Report({ routeId }: { routeId: number }) {
  const [report, setReport] = useState<RouteReport | null>(null);
  const pushToast = useAppStore((s) => s.pushToast);
  useEffect(() => {
    void api.getRouteReport(routeId).then(setReport).catch((e) => toastError(e));
  }, [routeId]);
  if (!report) return <div className="muted small">Building report…</div>;
  const exportAs = async (format: "csv" | "geojson") => {
    try {
      const path = await api.exportRouteReport(routeId, format);
      if (path) pushToast(`Report written to ${path}`, "success");
    } catch (e) {
      toastError(e, "Export failed");
    }
  };
  return (
    <div className="history" style={{ maxHeight: 320 }}>
      <div className="callout honesty small">{report.disclaimer}</div>
      <div className="muted small" style={{ marginBottom: 6 }}>
        Route length {formatDistance(report.length_m)} · corridor ±{report.route.corridor_m} m · {report.cameras.length} camera(s) known · generated {formatTime(report.generated_at)}
      </div>
      <div className="row wrap" style={{ marginBottom: 6 }}>
        <button className="btn small" onClick={() => void exportAs("csv")}>
          Export CSV
        </button>
        <button className="btn small" onClick={() => void exportAs("geojson")}>
          Export GeoJSON
        </button>
        <button className="btn small" onClick={() => showCamerasOnMap(report.cameras.map((c) => c.camera))}>
          Show on map
        </button>
      </div>
      {report.cameras.length > 0 && (
        <table className="report">
          <thead>
            <tr>
              <th>Along</th>
              <th>Off route</th>
              <th>Category</th>
              <th>Operator</th>
            </tr>
          </thead>
          <tbody>
            {report.cameras.map((c) => (
              <tr key={`${c.camera.osm_type}/${c.camera.osm_id}`} style={{ cursor: "pointer" }} onClick={() => showCamerasOnMap([c.camera])}>
                <td>{formatDistance(c.along_m)}</td>
                <td>{formatDistance(c.distance_m)}</td>
                <td>{c.camera.category}</td>
                <td>{c.camera.tags.operator ?? c.camera.tags.brand ?? ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function overlapNote(me: AreaSummary, all: AreaSummary[]): string | null {
  const mine = new Set(me.camera_keys);
  const parts: string[] = [];
  for (const other of all) {
    if (other.area.id === me.area.id) continue;
    const shared = other.camera_keys.filter((k) => mine.has(k)).length;
    if (shared > 0) parts.push(`${shared} also in “${other.area.name}”`);
  }
  return parts.length ? `Overlap: ${parts.join(", ")} (each area notifies separately).` : null;
}

export default function AlertsPanel() {
  const alertState = useAppStore((s) => s.alertState);
  const setPanel = useAppStore((s) => s.setPanel);
  const setMode = useAppStore((s) => s.setMode);
  const setPendingRoute = useAppStore((s) => s.setPendingRoute);
  const pushToast = useAppStore((s) => s.pushToast);
  const flyTo = useAppStore((s) => s.flyTo);
  const [open, setOpen] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [renaming, setRenaming] = useState<{ id: number; name: string } | null>(null);

  useEffect(() => {
    void api.acknowledgeAlerts().then(reloadAlertState).catch(() => reloadAlertState());
  }, []);

  const refresh = async () => {
    setRefreshing(true);
    try {
      const o = await api.runAlertRefresh();
      if (o.offline) pushToast("Offline — refresh skipped and will retry at the next interval.", "warn");
      else {
        const added = o.targets.reduce((n, t) => n + (t.baseline ? 0 : t.added.length), 0);
        pushToast(added > 0 ? `${added} newly mapped camera(s) found.` : "No new cameras since the last check.", added > 0 ? "warn" : "success");
      }
      await reloadAlertState();
    } catch (e) {
      toastError(e, "Refresh failed");
    } finally {
      setRefreshing(false);
    }
  };

  const importGpx = async () => {
    try {
      const g = await api.importGpx();
      if (!g) return;
      setPendingRoute({ points: g.points, name: g.name, lengthM: g.length_m, source: "gpx" });
    } catch (e) {
      toastError(e, "GPX import failed");
    }
  };

  const deleteArea = async (a: AreaSummary) => {
    try {
      await api.deleteWatchArea(a.area.id);
      await reloadAlertState();
    } catch (e) {
      toastError(e);
    }
  };
  const deleteRoute = async (r: RouteSummary) => {
    try {
      await api.deleteRoute(r.route.id);
      await reloadAlertState();
    } catch (e) {
      toastError(e);
    }
  };
  const rename = async () => {
    if (!renaming) return;
    try {
      await api.renameWatchArea(renaming.id, renaming.name);
      setRenaming(null);
      await reloadAlertState();
    } catch (e) {
      toastError(e);
    }
  };

  const toggle = (key: string) => setOpen(open === key ? null : key);

  return (
    <div className="panel">
      <div className="panel-header">
        <span>Alerts</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        <div className="callout honesty">{alertState?.disclaimer ?? "Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence of a camera."}</div>
        <div className="muted small" style={{ marginBottom: 10 }}>
          Alerts compare saved places and routes against OpenStreetMap over time. They never use your live position.
        </div>
        <div className="row wrap" style={{ marginBottom: 8 }}>
          <button className="btn small primary" disabled={refreshing || alertState?.refresh_running} onClick={() => void refresh()}>
            {refreshing || alertState?.refresh_running ? "Refreshing…" : "Check now"}
          </button>
          <button className="btn small" onClick={() => { setMode("draw"); setPanel("none"); }}>
            Draw a route
          </button>
          <button className="btn small" onClick={() => void importGpx()}>
            Import GPX
          </button>
        </div>
        <div className="muted small" style={{ marginBottom: 14 }}>
          Last check: {formatAgo(alertState?.last_refresh)}
          {alertState?.last_skipped_offline ? ` · last skipped (offline): ${formatAgo(alertState.last_skipped_offline)}` : ""}
          <br />
          To add a watch area, right-click (or long-press) the map, or use “Watch this area” on a camera.
        </div>

        <div className="section">
          <h4>Watch areas ({alertState?.areas.length ?? 0})</h4>
          {(alertState?.areas ?? []).length === 0 && <div className="muted small">None yet.</div>}
          <div className="list">
            {(alertState?.areas ?? []).map((a) => {
              const key = `area-${a.area.id}`;
              const note = overlapNote(a, alertState?.areas ?? []);
              return (
                <div key={key} className="list-item">
                  {renaming?.id === a.area.id ? (
                    <div className="row">
                      <input className="input grow" value={renaming.name} onChange={(e) => setRenaming({ ...renaming, name: e.target.value })} onKeyDown={(e) => e.key === "Enter" && void rename()} />
                      <button className="btn small" onClick={() => void rename()}>
                        Save
                      </button>
                      <button className="btn small" onClick={() => setRenaming(null)}>
                        ✕
                      </button>
                    </div>
                  ) : (
                    <div className="title">
                      {a.area.name}
                      <span className="badge neutral">{a.count} known</span>
                    </div>
                  )}
                  <div className="muted small">
                    radius {formatDistance(a.area.radius_m)} · checked {formatAgo(a.area.last_checked)}
                    {!a.baseline_recorded && " · baseline pending (offline at creation)"}
                  </div>
                  {note && <div className="small" style={{ color: "#fde68a" }}>{note}</div>}
                  <div className="row wrap" style={{ marginTop: 6 }}>
                    <button className="btn small" onClick={() => flyTo({ lat: a.area.lat, lon: a.area.lon, zoom: 14 })}>
                      Go to
                    </button>
                    <button className="btn small" onClick={() => void showCameraKeysOnMap(a.camera_keys)}>
                      Show cameras
                    </button>
                    <button className="btn small" onClick={() => toggle(key)}>
                      {open === key ? "Hide history" : "History"}
                    </button>
                    <button className="btn small" onClick={() => setRenaming({ id: a.area.id, name: a.area.name })}>
                      Rename
                    </button>
                    <button className="btn small danger" onClick={() => void deleteArea(a)}>
                      Delete
                    </button>
                  </div>
                  {open === key && <History type="area" id={a.area.id} />}
                </div>
              );
            })}
          </div>
        </div>

        <div className="section">
          <h4>Routes ({alertState?.routes.length ?? 0})</h4>
          {(alertState?.routes ?? []).length === 0 && <div className="muted small">None yet. Draw one on the map or import a GPX track.</div>}
          <div className="list">
            {(alertState?.routes ?? []).map((r) => {
              const key = `route-${r.route.id}`;
              const rkey = `report-${r.route.id}`;
              return (
                <div key={key} className="list-item">
                  <div className="title">
                    {r.route.name}
                    <span className="badge neutral">{r.count} known</span>
                  </div>
                  <div className="muted small">
                    corridor ±{r.route.corridor_m} m · checked {formatAgo(r.route.last_checked)}
                    {!r.baseline_recorded && " · baseline pending (offline at creation)"}
                  </div>
                  <div className="row wrap" style={{ marginTop: 6 }}>
                    <button className="btn small" onClick={() => toggle(rkey)}>
                      {open === rkey ? "Hide report" : "Report"}
                    </button>
                    <button className="btn small" onClick={() => void showCameraKeysOnMap(r.camera_keys)}>
                      Show cameras
                    </button>
                    <button className="btn small" onClick={() => toggle(key)}>
                      {open === key ? "Hide history" : "History"}
                    </button>
                    <button className="btn small danger" onClick={() => void deleteRoute(r)}>
                      Delete
                    </button>
                  </div>
                  {open === rkey && <Report routeId={r.route.id} />}
                  {open === key && <History type="route" id={r.route.id} />}
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
