import { useEffect, useState } from "react";
import { toastError } from "../lib/actions";
import {
  cameraCount,
  coordLabel,
  formatRouteDistance,
  isAndroid,
  planHeadlines,
  REMAINING_TEXT,
  roadMapNote,
  routingErrorMessage,
  summaryLine,
  usesMiles,
  type Endpoint,
  type RouteChoice,
} from "../lib/directions";
import { api, onRouteProgress } from "../lib/ipc";
import { TYPING_LOCK_MPS, type Blocker } from "../lib/nav";
import { startNavigation, StartProblemCallout } from "./NavigationView";
import type { GeocodeResult, PlannedCamera, RoutePlan } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import { useSheet } from "./useSheet";

type Which = "start" | "end";

const DEFAULT_SERVER = "valhalla1.openstreetmap.de";

/** Host of the configured routing server, for messages. */
function serverName(endpoint: string | undefined): string {
  try {
    return endpoint ? new URL(endpoint).host : DEFAULT_SERVER;
  } catch {
    return endpoint || DEFAULT_SERVER;
  }
}

/** "39.74, -104.99" typed into a field needs no geocoding. */
function parseCoords(text: string): Endpoint | null {
  const m = text.trim().match(/^(-?\d{1,2}(?:\.\d+)?)\s*[,\s]\s*(-?\d{1,3}(?:\.\d+)?)$/);
  if (!m) return null;
  const lat = Number(m[1]);
  const lon = Number(m[2]);
  if (Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  return { lat, lon, label: coordLabel(lat, lon) };
}

/** Plans in flight; only the latest one's answer is shown. */
let planSeq = 0;

async function selectPlannedCamera(c: PlannedCamera) {
  const store = useAppStore.getState();
  store.flyTo({ lat: c.lat, lon: c.lon, zoom: 17 });
  if (c.source === "submission") {
    const id = Number(c.key.split("/")[1]);
    const submission = store.submissions.find((s) => s.id === id);
    if (submission) store.select({ kind: "submission", submission });
    return;
  }
  try {
    const [camera] = await api.getCamerasByKeys([c.key]);
    if (camera) store.select({ kind: "camera", camera });
  } catch (e) {
    toastError(e, "Could not open camera");
  }
}

function RouteCard({ plan, choice, selected, onSelect, miles }: { plan: RoutePlan; choice: RouteChoice; selected: boolean; onSelect: () => void; miles: boolean }) {
  const route = plan[choice];
  return (
    <button className={`route-card ${choice} ${selected ? "selected" : ""}`} onClick={onSelect} aria-pressed={selected}>
      <span className={`route-swatch ${choice}`} aria-hidden="true" />
      <span className="grow">
        <span className="route-card-title">{plan.same_route ? "Fastest = Avoidance" : choice === "avoid" ? "Avoidance" : "Fastest"}</span>
        <span className="route-card-line">{summaryLine(choice, plan, miles).replace(/^[^:]+:\s*/, "")}</span>
      </span>
      <span className={`route-cams ${route.cameras.length === 0 ? "zero" : ""}`}>{route.cameras.length}</span>
    </button>
  );
}

export default function DirectionsPanel() {
  const sheet = useSheet();
  const setPanel = useAppStore((s) => s.setPanel);
  const directions = useAppStore((s) => s.directions);
  const setDirections = useAppStore((s) => s.setDirections);
  const setEndpoint = useAppStore((s) => s.setEndpoint);
  const startPick = useAppStore((s) => s.startPick);
  const clearDirections = useAppStore((s) => s.clearDirections);
  const mode = useAppStore((s) => s.mode);
  const flyTo = useAppStore((s) => s.flyTo);
  const routingEndpoint = useAppStore((s) => s.settings?.routing_endpoint);
  const server = serverName(routingEndpoint);
  const miles = usesMiles();
  const { start, end, plan, selected, busy, progress, error } = directions;
  const debugBuild = useAppStore((s) => s.info?.debug_build ?? false);
  // No typing while driving: above 5 mph the address fields lock (the map and "Me" still work).
  const speed = useAppStore((s) => s.userPosition?.speed ?? 0);
  const typingLocked = (speed ?? 0) > TYPING_LOCK_MPS;
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<{ text: string; blocker?: Blocker } | null>(null);

  // Field text follows the endpoint whenever it is set elsewhere (map pick, swap, context menu).
  const [text, setText] = useState<Record<Which, string>>({ start: start?.label ?? "", end: end?.label ?? "" });
  const [choices, setChoices] = useState<Record<Which, GeocodeResult[] | null>>({ start: null, end: null });
  const [searching, setSearching] = useState<Which | null>(null);
  const [locating, setLocating] = useState(false);
  useEffect(() => setText((t) => ({ ...t, start: start?.label ?? "" })), [start]);
  useEffect(() => setText((t) => ({ ...t, end: end?.label ?? "" })), [end]);

  useEffect(() => {
    const un = onRouteProgress((p) => useAppStore.getState().setDirections({ progress: p }));
    return () => void un.then((f) => f());
  }, []);

  const endpointOf = (which: Which) => (which === "start" ? start : end);
  const fieldName = (which: Which) => (which === "start" ? "start" : "destination");

  /** The endpoint for a field, geocoding its text if it was edited. Null when the user must choose or fix it. */
  const resolve = async (which: Which): Promise<Endpoint | null> => {
    const current = endpointOf(which);
    const q = text[which].trim();
    if (current && q === current.label) return current;
    if (!q) {
      setDirections({ error: `Enter a ${fieldName(which)}, or pick it on the map.` });
      return null;
    }
    const typed = parseCoords(q);
    if (typed) {
      setEndpoint(which, typed);
      return typed;
    }
    setSearching(which);
    try {
      const results = await api.geocode(q);
      if (results.length === 0) {
        setDirections({ error: `No places found for "${q}". Try a fuller address, or pick the ${fieldName(which)} on the map.` });
        return null;
      }
      if (results.length === 1) {
        const e = { lat: results[0].lat, lon: results[0].lon, label: results[0].display_name };
        setEndpoint(which, e);
        return e;
      }
      setChoices((c) => ({ ...c, [which]: results }));
      setDirections({ error: null });
      return null;
    } catch (e) {
      setDirections({ error: `Address search failed: ${routingErrorMessage(e, "Nominatim")}` });
      return null;
    } finally {
      setSearching(null);
    }
  };

  const choose = (which: Which, r: GeocodeResult) => {
    setChoices((c) => ({ ...c, [which]: null }));
    setEndpoint(which, { lat: r.lat, lon: r.lon, label: r.display_name });
  };

  const getRoute = async () => {
    if (busy) return;
    const s = await resolve("start");
    if (!s) return;
    const e = await resolve("end");
    if (!e) return;
    const seq = ++planSeq;
    setDirections({ busy: true, error: null, progress: null });
    try {
      const result = await api.planRoute({ lat: s.lat, lon: s.lon }, { lat: e.lat, lon: e.lon });
      if (seq !== planSeq) return;
      setDirections({ plan: result, selected: "avoid", busy: false, progress: null });
      const b = [result.fastest.bbox, result.avoid.bbox];
      flyTo({
        bbox: {
          south: Math.min(b[0].south, b[1].south),
          west: Math.min(b[0].west, b[1].west),
          north: Math.max(b[0].north, b[1].north),
          east: Math.max(b[0].east, b[1].east),
        },
      });
    } catch (err) {
      if (seq !== planSeq) return;
      // The map keeps whatever route it was showing.
      // Named from the settings now, not at render time (they may have just changed).
      const host = serverName(useAppStore.getState().settings?.routing_endpoint);
      setDirections({ busy: false, progress: null, error: routingErrorMessage(err, host) });
    }
  };

  const useMyLocation = () => {
    const pos = useAppStore.getState().userPosition;
    if (pos && Date.now() - pos.at < 60_000) {
      setEndpoint("start", { lat: pos.lat, lon: pos.lon, label: "My location" });
      return;
    }
    if (!("geolocation" in navigator)) {
      setDirections({ error: "Location isn't available on this device." });
      return;
    }
    setLocating(true);
    navigator.geolocation.getCurrentPosition(
      (p) => {
        setLocating(false);
        setEndpoint("start", { lat: p.coords.latitude, lon: p.coords.longitude, label: "My location" });
      },
      (err) => {
        setLocating(false);
        setDirections({
          error:
            err.code === 1
              ? "Location permission was denied. Allow location for Flock Finder, or enter a start."
              : "Couldn't get your location. Enter a start or pick it on the map.",
        });
      },
      { enableHighAccuracy: true, timeout: 15000, maximumAge: 30000 },
    );
  };

  const swap = () => {
    setDirections({ start: end, end: start });
    setText({ start: text.end, end: text.start });
    setChoices({ start: choices.end, end: choices.start });
  };

  const field = (which: Which, label: string) => (
    <div className="field">
      <label htmlFor={`dir-${which}`}>{label}</label>
      <div className="row">
        <input
          id={`dir-${which}`}
          className="input grow"
          value={text[which]}
          placeholder={typingLocked ? "Stop to type an address" : searching === which ? "Searching…" : "Address, place or lat, lon"}
          disabled={typingLocked}
          onChange={(e) => {
            const v = e.target.value;
            setText((t) => ({ ...t, [which]: v }));
            setChoices((c) => ({ ...c, [which]: null }));
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void resolve(which);
          }}
        />
        <button
          className={`btn small ${mode === "pick" && directions.picking === which ? "active" : ""}`}
          onClick={() => startPick(which)}
          title={`Pick the ${fieldName(which)} on the map`}
        >
          Map
        </button>
        {which === "start" && isAndroid() && (
          <button className="btn small" onClick={useMyLocation} disabled={locating} title="Start from your current location">
            {locating ? "…" : "Me"}
          </button>
        )}
      </div>
      {choices[which] && (
        <div className="choices" role="listbox" aria-label={`Matches for the ${fieldName(which)}`}>
          <span className="muted small">Several places match. Pick one:</span>
          {choices[which]!.map((r, i) => (
            <button key={i} onClick={() => choose(which, r)}>
              {r.display_name}
            </button>
          ))}
        </div>
      )}
    </div>
  );

  const route = plan?.[selected];
  const navChoice = plan?.same_route ? "avoid" : selected;
  const canNavigate = isAndroid() || debugBuild;
  const beginNavigation = async (simulate: boolean, replayGpx = false) => {
    if (!plan || !end) return;
    setStarting(true);
    setStartError(null);
    const err = await startNavigation(navChoice, plan[navChoice], { lat: end.lat, lon: end.lon, label: end.label }, { simulate, replayGpx });
    setStarting(false);
    setStartError(err);
  };
  return (
    <div ref={sheet.ref} className={`panel directions ${mode === "pick" ? "picking" : ""} ${sheet.className}`}>
      <div className="panel-header" {...sheet.headerProps}>
        {sheet.grip}
        <span>Directions</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        <div className="section">
          {field("start", "Start")}
          <div className="row between swap-row">
            <button className="btn small" onClick={swap} title="Swap start and destination" disabled={!start && !end && !text.start && !text.end}>
              ⇅ Swap
            </button>
          </div>
          {field("end", "Destination")}
          <div className="row">
            <button className="btn primary grow" onClick={() => void getRoute()} disabled={busy}>
              {busy ? "Routing…" : "Get route"}
            </button>
            <button className="btn" onClick={() => { planSeq++; clearDirections(); setText({ start: "", end: "" }); setChoices({ start: null, end: null }); }}>
              Clear
            </button>
          </div>
          {busy && (
            <div className="muted small route-progress" role="status">
              {progress ? `${progress.message}… (request ${progress.step}, up to ${progress.max})` : "Starting…"}
            </div>
          )}
          <p className="muted small">
            Your start and destination go to {server}
            {server === DEFAULT_SERVER ? " (a public routing server run by FOSSGIS e.V.)" : " (the routing server set in Settings)"}, along
            with the cameras being routed around. Typed addresses go to Nominatim. If the first search
            can't avoid every camera, the road map for the trip area is downloaded from OpenStreetMap
            (Overpass), which sees that area; it's saved on this device for 30 days.
          </p>
        </div>

        {error && <div className="callout error" role="alert">{error}</div>}

        {plan && route && (
          <>
            {planHeadlines(plan).map((h, i) => (
              <div key={i} className={`callout ${h.tone === "success" ? "success" : h.tone}`}>
                {h.text}
              </div>
            ))}
            {roadMapNote(plan) && <p className="muted small">{roadMapNote(plan)}</p>}
            <div className="section route-cards">
              {plan.same_route ? (
                <RouteCard plan={plan} choice="avoid" selected onSelect={() => {}} miles={miles} />
              ) : (
                (["avoid", "fastest"] as RouteChoice[]).map((c) => (
                  <RouteCard key={c} plan={plan} choice={c} selected={selected === c} onSelect={() => setDirections({ selected: c })} miles={miles} />
                ))
              )}
            </div>

            {canNavigate && (
              <div className="section nav-start">
                {isAndroid() && (
                  <button className="btn primary nav-big" onClick={() => void beginNavigation(false)} disabled={starting}>
                    {starting ? "Starting…" : `Start ${plan.same_route ? "" : navChoice === "avoid" ? "avoidance " : "fastest "}navigation`}
                  </button>
                )}
                {debugBuild && (
                  <div className="row">
                    <button className="btn small" onClick={() => void beginNavigation(true)} disabled={starting} title="Debug builds: drive this route with the simulator">
                      Simulate drive
                    </button>
                    <button className="btn small" onClick={() => void beginNavigation(true, true)} disabled={starting} title="Debug builds: replay a recorded GPX track">
                      Replay GPX…
                    </button>
                  </div>
                )}
                {startError && <StartProblemCallout problem={startError} onFixed={() => setStartError(null)} />}
              </div>
            )}

            <div className="section">
              <h4>
                {selected === "avoid" && !plan.same_route ? "Avoidance route" : plan.same_route ? "Route" : "Fastest route"}: {cameraCount(route.cameras.length)}
              </h4>
              {route.cameras.length === 0 ? (
                <p className="muted small">No mapped or submitted cameras within 30 m of this route.</p>
              ) : (
                <div className="list">
                  {route.cameras.map((c) => (
                    <button key={c.key} className="list-item route-camera" onClick={() => void selectPlannedCamera(c)}>
                      <span className="title">
                        <span className="route-cam-dot" aria-hidden="true" />
                        {c.category === "flock" ? "Flock camera" : "ALPR camera"}
                        {c.source === "submission" && <span className="badge neutral">unverified</span>}
                        <span className="grow" />
                        <span className="muted small">at {formatRouteDistance(c.along_m, miles)}</span>
                      </span>
                      {c.operator && <span className="muted small">{c.operator}</span>}
                      {c.remaining && <span className="small remaining">{REMAINING_TEXT[c.remaining]}</span>}
                    </button>
                  ))}
                </div>
              )}
            </div>

            <div className="section">
              <h4>Turn by turn</h4>
              <ol className="steps">
                {route.maneuvers.map((m, i) => (
                  <li key={i}>
                    <button onClick={() => flyTo({ lat: m.lat, lon: m.lon, zoom: 17 })}>
                      <span className="grow">{m.instruction}</span>
                      {m.distance_m > 0 && <span className="muted small">{formatRouteDistance(m.distance_m, miles)}</span>}
                    </button>
                  </li>
                ))}
              </ol>
            </div>
            <p className="muted small">
              Cameras come from crowdsourced data and coverage is incomplete. A route shown here can still
              pass cameras nobody has mapped. The route isn't recomputed when camera data changes.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
