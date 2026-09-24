import { useEffect } from "react";
import AlertsPanel from "./components/AlertsPanel";
import DetailPanel from "./components/DetailPanel";
import FilterPanel from "./components/FilterPanel";
import MapView from "./components/MapView";
import OsmUploadDialog from "./components/OsmUploadDialog";
import ProximityAlerts from "./components/ProximityAlerts";
import { EmptyState, Hud, Legend } from "./components/MapHud";
import { ContextMenu, FirstRunDialog, Footer, StatusChips, Toasts } from "./components/Overlays";
import RouteDialog from "./components/RouteDialog";
import SettingsPanel from "./components/SettingsPanel";
import SubmissionForm from "./components/SubmissionForm";
import SubmissionsPanel from "./components/SubmissionsPanel";
import Toolbar, { ModeBar } from "./components/Toolbar";
import WatchAreaDialog from "./components/WatchAreaDialog";
import { reloadAlertState, reloadSubmissions, showCameraKeysOnMap, toastError } from "./lib/actions";
import { loadDataset } from "./lib/dataset";
import { api, inTauri, onAlertsRefreshed, onCamerasChanged, onOsmAuth, onSyncStatus } from "./lib/ipc";
import { useAppStore } from "./store/useAppStore";

/** StrictMode runs mount effects twice in development; the dataset must load only once. */
let booted = false;

export default function App() {
  const panel = useAppStore((s) => s.panel);
  const info = useAppStore((s) => s.info);

  useEffect(() => {
    if (!inTauri()) {
      useAppStore.getState().pushToast("Not running inside Tauri: the map needs the Rust backend. Start with `npm run tauri dev`.", "error");
      return;
    }
    const store = useAppStore.getState();
    // Cameras come from the local store; loading them does not wait for anything else.
    if (!booted) {
      booted = true;
      void loadDataset();
      void api
        .getSyncStatus()
        .then((s) => store.setSync(s))
        .catch((e) => toastError(e, "Could not read sync status"));
    }
    (async () => {
      try {
        const [appInfo, settings, view] = await Promise.all([api.getAppInfo(), api.getSettings(), api.getInitialView()]);
        store.setInfo(appInfo);
        store.setSettings(settings);
        store.setView(view);
        store.setFirstRunOpen(appInfo.first_run);
      } catch (e) {
        toastError(e, "Startup failed");
      }
      await reloadSubmissions();
      await reloadAlertState();
    })();

    const unlisteners: Promise<() => void>[] = [
      onSyncStatus((s) => store.setSync(s)),
      onCamerasChanged(() => void loadDataset()),
      onAlertsRefreshed((o) => {
        void reloadAlertState();
        for (const t of o.targets) {
          if (t.baseline || t.added.length === 0) continue;
          const keys = t.added.map(([type, id]) => `${type}/${id}`);
          const where = t.target_type === "area" ? "near" : "along";
          store.pushToast(`${t.added.length} new ALPR camera${t.added.length === 1 ? "" : "s"} ${where} ${t.name}.`, "warn", {
            label: "Show",
            run: () => void showCameraKeysOnMap(keys),
          });
        }
      }),
      onOsmAuth((o) => store.pushToast(o.message, o.ok ? "success" : "error")),
    ];
    const onOffline = () => store.setOffline(true);
    window.addEventListener("offline", onOffline);
    return () => {
      window.removeEventListener("offline", onOffline);
      for (const u of unlisteners) void u.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const s = useAppStore.getState();
      if (s.contextMenu) s.setContextMenu(null);
      else if (s.mode !== "view") s.setMode("view");
      else if (s.selection) s.select(null);
      else if (s.panel !== "none") s.setPanel("none");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <div className="map-wrap">
        <MapView />
        <Toolbar />
        <ModeBar />
        <ProximityAlerts />
        <EmptyState />
        <StatusChips />
        <div className="map-corner">
          <Legend />
          <Hud />
        </div>
        {panel === "filters" && <FilterPanel />}
        {panel === "submissions" && <SubmissionsPanel />}
        {panel === "alerts" && <AlertsPanel />}
        {panel === "settings" && <SettingsPanel />}
        <DetailPanel />
        <ContextMenu />
        <SubmissionForm key={useAppStore((s) => s.submissionDraft?.id ?? (s.submissionDraft ? "new" : "none"))} />
        <WatchAreaDialog />
        <RouteDialog />
        <OsmUploadDialog />
        <Toasts />
        {info && <FirstRunDialog />}
      </div>
      <Footer />
    </div>
  );
}
