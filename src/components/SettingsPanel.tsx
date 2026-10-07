import { useEffect, useState } from "react";
import { copyText, openOsm, reloadSettings, requestAreaRefresh, runSync, toastError } from "../lib/actions";
import { DETOUR_LIMIT_CHOICES } from "../lib/directions";
import { formatAgo, formatDistance, formatTime } from "../lib/geo";
import { api } from "../lib/ipc";
import { RADIUS_CHOICES } from "../lib/proximity";
import { DEFAULT_DARK, DEFAULT_LIGHT, THEME_IDS, THEMES, isThemeChoice, type ThemeChoice } from "../map/themes";
import { systemPrefersDark, useThemeChoice } from "../map/useMapStyle";
import type { CacheStats, OsmAuthStatus, Settings, WifiDatasetStatus, WifiIngestResult } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import { useSheet } from "./useSheet";

function ingestSummary(r: WifiIngestResult): string {
  const s = r.stats;
  return `${r.inserted} sighting(s) stored (${s.parsed} parsed, ${s.skipped_old} stale, ${s.skipped_invalid} invalid, ${s.skipped_unmatched} not Flock-like, ${s.deduplicated} duplicates).`;
}

/** The routing server's host, as set (empty: the default public server). */
function serverHost(endpoint: string): string {
  const e = endpoint.trim();
  if (!e) return "valhalla1.openstreetmap.de (FOSSGIS e.V.)";
  try {
    return new URL(e).host;
  } catch {
    return e;
  }
}

const REFRESH_OPTIONS: { value: number; label: string }[] = [
  { value: 0, label: "Manual only (no background requests)" },
  { value: 6, label: "Every 6 hours" },
  { value: 12, label: "Every 12 hours" },
  { value: 24, label: "Every 24 hours (default)" },
  { value: 48, label: "Every 2 days" },
  { value: 168, label: "Every 7 days" },
];

export default function SettingsPanel() {
  const sheet = useSheet();
  const settings = useAppStore((s) => s.settings);
  const info = useAppStore((s) => s.info);
  const setPanel = useAppStore((s) => s.setPanel);
  const pushToast = useAppStore((s) => s.pushToast);
  const flyTo = useAppStore((s) => s.flyTo);
  const proximity = useAppStore((s) => s.proximity);
  const setProximity = useAppStore((s) => s.setProximity);
  const sync = useAppStore((s) => s.sync);
  const themeChoice = useThemeChoice();
  const setMapTheme = useAppStore((s) => s.setMapTheme);
  const openPlaces = useAppStore((s) => s.openPlaces);
  const savedPlaces = useAppStore((s) => s.savedPlaces);
  const savedSummary = (() => {
    const list = savedPlaces ?? [];
    const set = [list.some((p) => p.kind === "home") && "Home", list.some((p) => p.kind === "work") && "Work"].filter(Boolean).join(" and ");
    const others = list.filter((p) => p.kind === "custom").length;
    if (!set && others === 0) return "None saved yet.";
    return [set && `${set} set`, others > 0 && `${others} other place${others === 1 ? "" : "s"}`].filter(Boolean).join(", ") + ".";
  })();
  const [form, setForm] = useState<Settings | null>(settings);
  const [saving, setSaving] = useState(false);
  const [auth, setAuth] = useState<OsmAuthStatus | null>(null);
  const [callbackUrl, setCallbackUrl] = useState("");
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [wifi, setWifi] = useState<WifiDatasetStatus | null>(null);
  const [wifiBusy, setWifiBusy] = useState<"download" | "import" | "clear" | null>(null);

  useEffect(() => setForm(settings), [settings]);

  const loadWifi = async () => {
    try {
      setWifi(await api.wifiDatasetStatus());
    } catch (e) {
      toastError(e);
    }
  };
  useEffect(() => {
    void loadWifi();
  }, []);

  const wifiDownload = async () => {
    setWifiBusy("download");
    try {
      const r = await api.wifiDownloadDataset();
      pushToast(`Wi-Fi fingerprint dataset updated: ${ingestSummary(r)}`, "success");
      await loadWifi();
      requestAreaRefresh();
    } catch (e) {
      toastError(e, "Dataset download failed");
    } finally {
      setWifiBusy(null);
    }
  };

  const wifiImport = async () => {
    setWifiBusy("import");
    try {
      const r = await api.wifiImportWigle();
      if (r) {
        pushToast(`Wigle import: ${ingestSummary(r)}`, r.inserted > 0 ? "success" : "warn");
        await loadWifi();
        requestAreaRefresh();
      }
    } catch (e) {
      toastError(e, "Wigle import failed");
    } finally {
      setWifiBusy(null);
    }
  };

  const wifiClear = async (source: "upstream" | "wigle_import" | null) => {
    setWifiBusy("clear");
    try {
      const n = await api.wifiClear(source);
      pushToast(`Removed ${n} Wi-Fi sighting(s).`, "success");
      await loadWifi();
      requestAreaRefresh();
    } catch (e) {
      toastError(e);
    } finally {
      setWifiBusy(null);
    }
  };

  const loadAuth = async () => {
    try {
      setAuth(await api.osmAuthStatus());
    } catch (e) {
      toastError(e);
    }
  };
  const loadCache = async () => {
    try {
      setCache(await api.getCacheStats());
    } catch (e) {
      toastError(e);
    }
  };
  useEffect(() => {
    void loadAuth();
    void loadCache();
  }, []);

  if (!form) return null;
  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => setForm({ ...form, [k]: v });

  const save = async () => {
    setSaving(true);
    try {
      const saved = await api.saveSettings(form);
      useAppStore.getState().setSettings(saved);
      pushToast("Settings saved.", "success");
      void loadAuth();
    } catch (e) {
      toastError(e, "Could not save settings");
    } finally {
      setSaving(false);
    }
  };

  const signIn = async () => {
    try {
      await api.saveSettings(form);
      await reloadSettings();
      await api.osmSignIn();
      pushToast("Your browser opened the OpenStreetMap sign-in page. Come back here when done.", "info");
      void loadAuth();
    } catch (e) {
      toastError(e, "Sign-in could not start");
    }
  };

  const completeManually = async () => {
    try {
      const r = await api.osmCompleteAuth(callbackUrl);
      pushToast(r.message, r.ok ? "success" : "error");
      setCallbackUrl("");
      void loadAuth();
    } catch (e) {
      toastError(e);
    }
  };

  const signOut = async () => {
    try {
      await api.osmSignOut();
      pushToast("Signed out of OpenStreetMap.", "success");
      void loadAuth();
    } catch (e) {
      toastError(e);
    }
  };

  const clearCache = async () => {
    try {
      await api.clearCache();
      pushToast("Alert-area stamps reset. Watch areas and routes re-query their cells on the next alert refresh.", "success");
      void loadCache();
    } catch (e) {
      toastError(e);
    }
  };

  const loadFixture = async () => {
    try {
      const r = await api.loadFixture();
      pushToast(`Loaded ${r.stats.elements} sample cameras (Denver) into the local store.`, "success");
      flyTo({ bbox: r.bbox });
      void loadCache();
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <div ref={sheet.ref} className={`panel ${sheet.className}`}>
      <div className="panel-header" {...sheet.headerProps}>
        {sheet.grip}
        <span>Settings</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        <div className="section">
          <h4>Data source</h4>
          <div className="field">
            <label>Camera sync source</label>
            <select className="select" value={form.sync_source} onChange={(e) => set("sync_source", e.target.value as Settings["sync_source"])}>
              <option value="snapshot">Daily snapshot (recommended)</option>
              <option value="overpass">Query Overpass directly</option>
            </select>
            <span className="muted small">
              {form.sync_source === "snapshot"
                ? "Every mapped camera, fetched from OpenStreetMap once a day by the project and published as one ~4 MB file. Checked daily; a check with nothing new downloads a few hundred bytes. Falls back to querying Overpass directly if no current snapshot is published."
                : "This device runs the worldwide Overpass query itself (about 200 s of server time on a shared, volunteer-run service) at the interval below."}
            </span>
          </div>
          {form.sync_source === "snapshot" && (
            <div className="field">
              <label>Snapshot manifest URL (optional)</label>
              <input
                className="input"
                value={form.snapshot_url}
                onChange={(e) => set("snapshot_url", e.target.value)}
                placeholder="Built-in (this app's GitHub release)"
              />
            </div>
          )}
          <div className="field">
            <label>Overpass endpoint</label>
            <input className="input" value={form.overpass_endpoint} onChange={(e) => set("overpass_endpoint", e.target.value)} />
            <span className="muted small">
              Default: https://overpass-api.de/api/interpreter. Alternates: https://overpass.kumi.systems/api/interpreter
            </span>
          </div>
          <div className="field">
            <label>Overpass sync interval (days, 1–30)</label>
            <input
              className="input"
              type="number"
              min={1}
              max={30}
              value={form.cache_ttl_days}
              onChange={(e) => set("cache_ttl_days", Number(e.target.value))}
            />
            <span className="muted small">
              When querying Overpass directly, every mapped camera in the world is downloaded in one request
              (about 57 MB) at most this often. Moving the map makes no network requests.
            </span>
          </div>
        </div>

        <div className="section">
          <h4>Basemap</h4>
          <div className="field">
            <label htmlFor="map-theme">Map theme</label>
            <select
              id="map-theme"
              className="select"
              value={themeChoice ?? "system"}
              onChange={(e) => {
                if (isThemeChoice(e.target.value)) setMapTheme(e.target.value as ThemeChoice);
              }}
            >
              <option value="system">
                Match system ({THEMES[systemPrefersDark(info) ? DEFAULT_DARK : DEFAULT_LIGHT].name} now)
              </option>
              {(["dark", "light"] as const).map((scheme) => (
                <optgroup key={scheme} label={scheme === "dark" ? "Dark" : "Light"}>
                  {THEME_IDS.filter((id) => THEMES[id].scheme === scheme).map((id) => (
                    <option key={id} value={id}>
                      {THEMES[id].name} · {THEMES[id].description}
                    </option>
                  ))}
                </optgroup>
              ))}
              <option value="custom">Custom style URL…</option>
            </select>
            <span className="muted small">
              Applies immediately and is remembered on this device. The map button with the half-filled
              circle steps through the themes. Map data by OpenFreeMap (keyless, no account).
            </span>
          </div>
          {themeChoice === "custom" && (
            <div className="field">
              <label>MapLibre style URL</label>
              <input className="input" value={form.style_url} onChange={(e) => set("style_url", e.target.value)} placeholder="https://…/style.json" />
              <span className="muted small">
                Any MapLibre style JSON URL; it is dimmed to sit behind the camera data. Save settings to
                apply. Leave blank for a plain background.
              </span>
            </div>
          )}
        </div>

        <div className="section">
          <h4>Alerts</h4>
          <div className="field">
            <label>Background refresh of watch areas and routes</label>
            <select className="select" value={form.refresh_interval_hours} onChange={(e) => set("refresh_interval_hours", Number(e.target.value))}>
              {REFRESH_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
            <span className="muted small">
              Also runs shortly after launch (skipped if the last refresh was under an hour ago).
            </span>
          </div>
          <label className="checkbox">
            <input type="checkbox" checked={form.notifications_enabled} onChange={(e) => set("notifications_enabled", e.target.checked)} />
            Show desktop notifications for newly mapped cameras
          </label>
        </div>

        <div className="section">
          <h4>Camera proximity alerts</h4>
          <label className="checkbox">
            <input type="checkbox" checked={proximity.enabled} onChange={(e) => setProximity({ enabled: e.target.checked })} />
            Alert me when I come near a mapped camera
          </label>
          <div className="field">
            <label>Alert distance</label>
            <select
              className="select"
              value={proximity.radiusM}
              disabled={!proximity.enabled}
              onChange={(e) => setProximity({ radiusM: Number(e.target.value) })}
            >
              {RADIUS_CHOICES.map((m) => (
                <option key={m} value={m}>
                  {formatDistance(m)}
                </option>
              ))}
            </select>
          </div>
          <label className="checkbox">
            <input type="checkbox" checked={proximity.sound} disabled={!proximity.enabled} onChange={(e) => setProximity({ sound: e.target.checked })} />
            Play a sound (and vibrate on phones)
          </label>
          <div className="muted small">
            Works while the locate button is on and the app is open, and keeps the screen on meanwhile.
            Your position is checked on this device against cameras of the categories shown in Filters;
            it is never stored or uploaded. Changes apply immediately.
          </div>
        </div>

        <div className="section">
          <h4>Saved places</h4>
          <div className="row between">
            <span className="muted small">
              {savedSummary}
            </span>
            <button className="btn small" onClick={() => openPlaces()}>
              Saved places…
            </button>
          </div>
          <span className="muted small">
            Home, Work and up to 10 other places, for one-tap directions from the bar at the top of the map. Kept on
            this device only.
          </span>
        </div>

        <div className="section">
          <h4>Directions</h4>
          <div className="field">
            <label>Routing server (Valhalla)</label>
            <input
              className="input"
              value={form.routing_endpoint}
              onChange={(e) => set("routing_endpoint", e.target.value)}
              placeholder="https://valhalla1.openstreetmap.de (default)"
            />
            <span className="muted small">
              Directions send your start and destination, and the cameras on the route being avoided, to this
              server. The default is the free public server run by FOSSGIS e.V. (fair use, rate limited). Point
              this at your own Valhalla instance to keep trips on your own machine.
            </span>
            <span className="muted small">
              Turn-by-turn navigation keeps your position on this device. Only when you leave the route does it
              send your current position (and your destination) to{" "}
              <strong>{serverHost(form.routing_endpoint)}</strong> to find a new route, at most once every 10
              seconds. No location history is stored; the destination of a trip in progress is kept only so the
              app can offer to resume it if Android closes it, and is deleted when the trip ends.
            </span>
          </div>
          <div className="field">
            <label htmlFor="detour-limit">Detour limit on long trips</label>
            <select
              id="detour-limit"
              className="select"
              value={form.max_detour_min_per_camera}
              onChange={(e) => set("max_detour_min_per_camera", Number(e.target.value))}
            >
              {DETOUR_LIMIT_CHOICES.map((m) => (
                <option key={m} value={m}>
                  {m === 0 ? "No limit (avoid every camera it can)" : `${m} minutes per camera avoided${m === 5 ? " (default)" : ""}`}
                </option>
              ))}
            </select>
            <span className="muted small">
              On trips over 60 km the avoidance route is fixed stretch by stretch around each group of cameras. A
              stretch is only rerouted when that adds at most this much driving time for each camera it avoids;
              otherwise the route keeps that road and its cameras are marked. A lower limit gives a quicker route
              that passes more cameras. Also used when rerouting during navigation.
            </span>
          </div>
        </div>

        <div className="section">
          <h4>OpenStreetMap upload (optional)</h4>
          <div className="muted small" style={{ marginBottom: 8 }}>
            Direct upload needs an OAuth 2 application registered on openstreetmap.org (Settings → OAuth 2
            applications) with redirect URI <code>{info?.osm_redirect_uri}</code>, scope <code>write_api</code>, and
            no client secret (public client with PKCE). Without it, use the JOSM export from the Submissions panel.
          </div>
          <div className="field">
            <label>OAuth client ID</label>
            <input className="input" value={form.osm_client_id} onChange={(e) => set("osm_client_id", e.target.value)} placeholder="paste the client ID" />
          </div>
          {auth && (
            <div className="row wrap" style={{ marginBottom: 8 }}>
              <span className="chip">{auth.signed_in ? "✓ Signed in" : auth.pending ? "Waiting for browser…" : "Not signed in"}</span>
              {!auth.signed_in && (
                <button className="btn small" disabled={form.osm_client_id.trim() === ""} onClick={() => void signIn()}>
                  Sign in with OSM
                </button>
              )}
              {auth.signed_in && (
                <button className="btn small" onClick={() => void signOut()}>
                  Sign out
                </button>
              )}
              {auth.keychain_error && <span className="callout error small">{auth.keychain_error}</span>}
            </div>
          )}
          {auth && auth.pending && !auth.signed_in && (
            <div className="field">
              <label>If the browser did not bring you back, paste the flockfinder:// callback URL here</label>
              <div className="row">
                <input className="input grow" value={callbackUrl} onChange={(e) => setCallbackUrl(e.target.value)} placeholder="flockfinder://oauth/callback?code=…&state=…" />
                <button className="btn small" disabled={!callbackUrl.trim()} onClick={() => void completeManually()}>
                  Finish
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="row" style={{ marginBottom: 18 }}>
          <button className="btn primary" disabled={saving} onClick={() => void save()}>
            Save settings
          </button>
          <button className="btn" onClick={() => setForm(settings)}>
            Revert
          </button>
        </div>

        <div className="section">
          <h4>Wi-Fi fingerprint dataset (optional)</h4>
          <div className="callout warn small">
            A second, heuristic layer: Wi-Fi radios whose MAC prefix (OUI) matches hardware associated with
            Flock Safety, as logged by volunteer wardrivers in WiGLE. Every point is <strong>suspected</strong>,
            not confirmed, and is shown separately from OSM cameras. It never feeds alerts or OSM uploads.
          </div>
          {wifi && (
            <div className="kv" style={{ marginBottom: 8 }}>
              <div className="k">Stored sightings</div>
              <div className="v">
                {wifi.total} ({wifi.upstream} from dataset, {wifi.imported} imported)
              </div>
              <div className="k">Dataset downloaded</div>
              <div className="v">{wifi.downloaded_at ? formatTime(wifi.downloaded_at) : "not yet"}</div>
              <div className="k">Upstream scan</div>
              <div className="v">
                {wifi.upstream_generated ? wifi.upstream_generated.slice(0, 10) : "—"}
                {wifi.upstream_total ? ` · ${wifi.upstream_total} records` : ""}
              </div>
              <div className="k">OUI prefixes</div>
              <div className="v">{wifi.oui_count} bundled · {wifi.retention_days}-day retention</div>
            </div>
          )}
          <div className="row wrap" style={{ marginBottom: 6 }}>
            <button className="btn small primary" disabled={wifiBusy !== null} onClick={() => void wifiDownload()} title="Downloads about 24 MB from GitHub">
              {wifiBusy === "download" ? "Downloading…" : wifi?.downloaded_at ? "Update dataset (≈24 MB)" : "Download dataset (≈24 MB)"}
            </button>
            <button className="btn small" disabled={wifiBusy !== null} onClick={() => void wifiImport()} title="WigleWifi CSV from the WiGLE app, ESP32 Marauder Flock Wardrive, etc.">
              {wifiBusy === "import" ? "Importing…" : "Import Wigle CSV…"}
            </button>
            {wifi && wifi.total > 0 && (
              <button className="btn small danger" disabled={wifiBusy !== null} onClick={() => void wifiClear(null)}>
                Remove all
              </button>
            )}
          </div>
          <div className="muted small">
            Data and OUI research: {" "}
            <a href="#" onClick={(e) => { e.preventDefault(); void openOsm(wifi?.repo_url ?? "https://github.com/simeononsecurity/flock-finder"); }}>
              simeononsecurity/flock-finder
            </a>{" "}
            (MIT; prefixes by @NitekryDPaul and DeFlockJoplin) built from{" "}
            <a href="#" onClick={(e) => { e.preventDefault(); void openOsm("https://wigle.net"); }}>WiGLE</a>. See its{" "}
            <a href="#" onClick={(e) => { e.preventDefault(); void openOsm(wifi?.policy_url ?? "https://github.com/simeononsecurity/flock-finder/blob/main/docs/DATA_POLICY.md"); }}>
              data policy
            </a>{" "}
            for provenance and corrections. Only local imports match your own scans; nothing you import is uploaded anywhere.
          </div>
        </div>

        <div className="section">
          <h4>Camera data</h4>
          {sync && (
            <div className="kv" style={{ marginBottom: 8 }}>
              <div className="k">Cameras on this device</div>
              <div className="v">
                {sync.cameras.toLocaleString("en-US")} ({sync.stale_cameras.toLocaleString("en-US")} missing from the latest sync, kept hollow for 30 days)
              </div>
              <div className="k">Last sync</div>
              <div className="v">
                {sync.last_ok_at ? `${formatTime(sync.last_ok_at)} (${formatAgo(sync.last_ok_at)})` : "never"}
                {sync.last_elements !== null && sync.last_duration_secs !== null
                  ? ` · ${sync.last_elements.toLocaleString("en-US")} elements in ${sync.last_duration_secs} s`
                  : ""}
              </div>
              <div className="k">Data</div>
              <div className="v">
                {sync.data_as_of
                  ? `OpenStreetMap as of ${formatTime(Date.parse(sync.data_as_of) / 1000)}${sync.data_source ? ` (via ${sync.data_source})` : ""}`
                  : "—"}
              </div>
              <div className="k">Latest attempt</div>
              <div className="v">
                {sync.running ? `running (${sync.phase})` : sync.last_outcome ?? "none"}
                {sync.last_error ? `: ${sync.last_error}` : ""}
              </div>
              <div className="k">Next automatic sync</div>
              <div className="v">{formatTime(sync.next_due_at)}</div>
              {cache && (
                <>
                  <div className="k">Alert-area cells</div>
                  <div className="v">{cache.cells}</div>
                </>
              )}
            </div>
          )}
          <div className="row wrap">
            <button className="btn small primary" disabled={!sync || sync.running} onClick={() => void runSync()}>
              {sync?.running ? "Syncing…" : "Sync now"}
            </button>
            <button className="btn small" onClick={() => void clearCache()} title="Watch areas and routes re-query their own cells on the next alert refresh">
              Reset alert-area stamps
            </button>
            <button className="btn small" onClick={() => void loadFixture()} title="Ingest the bundled Overpass sample (Denver) for offline development">
              Load sample data
            </button>
            <button className="btn small" onClick={() => void loadCache()}>
              Refresh stats
            </button>
          </div>
        </div>

        {info && (
          <div className="section">
            <h4>About</h4>
            <div className="kv">
              <div className="k">Version</div>
              <div className="v">{info.version}</div>
              <div className="k">User-Agent</div>
              <div className="v">
                <code>{info.user_agent}</code>
              </div>
              <div className="k">Database</div>
              <div className="v">
                <code style={{ wordBreak: "break-all" }}>{info.db_path}</code>{" "}
                <button className="btn small" onClick={() => void copyText(info.db_path)}>
                  Copy
                </button>
              </div>
            </div>
            <div className="muted small" style={{ marginTop: 8 }}>
              Camera data © OpenStreetMap contributors, ODbL. Geocoding by Nominatim. This app maps fixed
              hardware only and never handles plate, vehicle or personal data.
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
