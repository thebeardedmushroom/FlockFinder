import { useEffect, useState } from "react";
import { toastError } from "../lib/actions";
import { routingErrorMessage } from "../lib/directions";
import {
  BLOCKER_TEXT,
  blockerFor,
  DEFAULT_SIM,
  navApi,
  navClock,
  navCovered,
  navDistance,
  navDuration,
  STATE_LABEL,
  stepDetail,
  type ActiveTrip,
  type Blocker,
  type NavDestination,
  type NavMode,
  type NavSession,
  type StepView,
} from "../lib/nav";
import { api } from "../lib/ipc";
import type { PlannedRoute } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import ManeuverIcon from "./ManeuverIcon";
import Modal from "./Modal";

/** The arrival summary closes itself after this long. */
const ARRIVAL_CLOSE_MS = 10_000;
/** A camera alert card stays up this long. */
const ALERT_SHOW_MS = 12_000;
/** The map's own proximity banner clears itself after this long while navigating. */
const PROXIMITY_CLEAR_MS = 15_000;

export async function refreshNavRoute() {
  try {
    const [route, covered] = await Promise.all([navApi.route(), navApi.coveredCameras()]);
    useAppStore.getState().setNavRoute(route);
    navCovered.clear();
    for (const k of covered) navCovered.add(k);
  } catch (e) {
    toastError(e, "Could not load the navigation route");
  }
}

/**
 * Make sure navigation can run (precise location, location on, notifications asked for), asking
 * as needed. Returns a message to show when it can't.
 */
async function ensureReady(simulated: boolean): Promise<{ blocker: Blocker; text: string } | null> {
  let r = await navApi.readiness();
  let b = blockerFor(r, simulated);
  if (b?.kind === "permission") {
    r = await navApi.request("request_location");
    b = blockerFor(r, simulated);
  }
  if (b?.kind === "location_off") {
    r = await navApi.request("enable_location");
    b = blockerFor(r, simulated);
  }
  if (b) return { blocker: b, text: BLOCKER_TEXT[b.kind] };
  if (r.platform === "android" && !r.notifications) {
    // Guidance works without it; the notification just won't show.
    try {
      await navApi.request("request_notifications");
    } catch {
      /* ignore */
    }
  }
  return null;
}

export interface StartOptions {
  simulate?: boolean;
  replayGpx?: boolean;
}

/** Start guidance along a planned route. Returns an error message, or null when it started. */
export async function startNavigation(
  mode: NavMode,
  route: PlannedRoute,
  destination: NavDestination,
  opts: StartOptions = {},
): Promise<StartProblem | null> {
  const store = useAppStore.getState();
  try {
    const problem = await ensureReady(!!opts.simulate);
    if (problem) return problem;
    const session = await navApi.start(mode, route, destination, opts.simulate ? DEFAULT_SIM : undefined, !!opts.replayGpx);
    if (!session) return null; // GPX pick cancelled
    store.setNav(session);
    store.setNavResume(null);
    store.setPanel("none");
    store.select(null);
    await refreshNavRoute();
    return null;
  } catch (e) {
    return { text: routingErrorMessage(e) };
  }
}

/** Why navigation couldn't start, and (when there is one) the blocker the user can fix. */
export type StartProblem = { text: string; blocker?: Blocker };

/** The message, with a button to the setting that fixes it (app settings, or location on). */
export function StartProblemCallout({ problem, onFixed }: { problem: StartProblem; onFixed: () => void }) {
  const b = problem.blocker;
  const fix = async () => {
    if (!b) return;
    try {
      await navApi.request(b.kind === "location_off" ? "enable_location" : "open_app_settings");
      onFixed();
    } catch (e) {
      toastError(e, "Could not open settings");
    }
  };
  return (
    <div className="callout warn" role="alert">
      {problem.text}
      {b && b.kind !== "desktop" && b.kind !== "permission" && (
        <div className="row">
          <button className="btn small" onClick={() => void fix()}>
            {b.kind === "location_off" ? "Turn on location" : "Open app settings"}
          </button>
        </div>
      )}
    </div>
  );
}

/** "Resume navigation to …?": plan again from where the phone is now, in the same mode. */
async function resumeTrip(trip: ActiveTrip): Promise<StartProblem | null> {
  const problem = await ensureReady(false);
  if (problem) return problem;
  const pos = await new Promise<GeolocationPosition | null>((resolve) =>
    navigator.geolocation.getCurrentPosition(resolve, () => resolve(null), { enableHighAccuracy: true, timeout: 20000, maximumAge: 10000 }),
  );
  if (!pos) return { text: "Couldn't get your location to resume navigation." };
  try {
    const plan = await api.planRoute({ lat: pos.coords.latitude, lon: pos.coords.longitude }, { lat: trip.destination.lat, lon: trip.destination.lon });
    const err = await startNavigation(trip.mode, plan[trip.mode], trip.destination);
    return err;
  } catch (e) {
    return { text: `Can't resume navigation: ${routingErrorMessage(e)}` };
  }
}

export function NavResumeDialog() {
  const trip = useAppStore((s) => s.navResume);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<StartProblem | null>(null);
  if (!trip) return null;
  const dismiss = () => {
    void navApi.forgetResume();
    useAppStore.getState().setNavResume(null);
  };
  const resume = async () => {
    setBusy(true);
    setError(null);
    const err = await resumeTrip(trip);
    setBusy(false);
    if (err) setError(err);
  };
  return (
    <Modal
      title="Resume navigation?"
      footer={
        <>
          <button className="btn nav-big" onClick={dismiss} disabled={busy}>
            No
          </button>
          <button className="btn primary nav-big" onClick={() => void resume()} disabled={busy}>
            {busy ? "Routing…" : "Resume"}
          </button>
        </>
      }
    >
      <p>
        Navigation to <strong>{trip.destination.label}</strong> stopped when Android closed Flock Finder. Resume it
        {trip.mode === "avoid" ? " (avoiding cameras)" : " (fastest route)"} from where you are now?
      </p>
      {error && <StartProblemCallout problem={error} onFixed={() => setError(null)} />}
    </Modal>
  );
}

function Step({ step, small }: { step: StepView; small?: boolean }) {
  const detail = stepDetail(step);
  return (
    <div className={small ? "nav-then" : "nav-step"}>
      <span className="nav-icon">
        <ManeuverIcon name={step.icon} size={small ? 28 : 56} />
      </span>
      <div className="grow">
        {!small && <div className="nav-dist">{navDistance(step.distance_m)}</div>}
        <div className="nav-instr">{small ? `Then ${step.instruction.replace(/^./, (c) => c.toLowerCase())}` : step.instruction}</div>
        {!small && detail && <div className="nav-street">{detail}</div>}
      </div>
    </div>
  );
}

function SimMenu({ nav }: { nav: NavSession }) {
  const [open, setOpen] = useState(false);
  const [mph, setMph] = useState(30);
  const cmd = (c: Parameters<typeof navApi.sim>[0]) => void navApi.sim(c).catch((e) => toastError(e, "Simulator"));
  if (!nav.simulated) return null;
  return (
    <div className={`nav-sim ${open ? "open" : ""}`}>
      <button className="btn small" onClick={() => setOpen(!open)} aria-expanded={open}>
        Simulator
      </button>
      {open && (
        <div className="nav-sim-body">
          <button className="btn small" onClick={() => cmd({ type: "deviate", meters: 400 })}>
            Leave the route
          </button>
          <button className="btn small" onClick={() => cmd({ type: "lose_signal", secs: 40 })}>
            Lose signal 40 s
          </button>
          <label className="row small">
            Speed {mph} mph
            <input
              type="range"
              min={0}
              max={75}
              step={5}
              value={mph}
              onChange={(e) => {
                const v = Number(e.target.value);
                setMph(v);
                cmd({ type: "speed", mps: v * 0.44704 });
              }}
            />
          </label>
          <div className="row small">
            Noise
            {[0, 4, 15].map((m) => (
              <button key={m} className="btn small" onClick={() => cmd({ type: "noise", meters: m })}>
                {m} m
              </button>
            ))}
          </div>
          <div className="row small">
            Rate
            {[1, 2, 4, 8].map((r) => (
              <button key={r} className="btn small" onClick={() => cmd({ type: "rate", rate: r })}>
                {r}×
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

/** End guidance (the End button, or closing the arrival summary). */
async function endNavigation() {
  try {
    await navApi.stop();
  } catch (e) {
    toastError(e, "Could not end navigation");
  }
  useAppStore.getState().setNav(null);
  navCovered.clear();
}

export default function NavigationView() {
  const nav = useAppStore((s) => s.nav);
  const follow = useAppStore((s) => s.navFollow);
  const northUp = useAppStore((s) => s.navNorthUp);
  const navRoute = useAppStore((s) => s.navRoute);
  const [voiceNoticeSeen, setVoiceNoticeSeen] = useState(false);

  // The screen stays on while this shows (and only then).
  const active = nav !== null;
  useEffect(() => {
    if (!active) return;
    void navApi.keepScreenOn(true).catch(() => {});
    return () => void navApi.keepScreenOn(false).catch(() => {});
  }, [active]);

  // Positions also feed the map's own proximity alerts (for cameras off the route).
  useEffect(() => {
    const p = nav?.position;
    if (!p) return;
    useAppStore.getState().setUserPosition({ lat: p.lat, lon: p.lon, accuracy: p.accuracy_m, speed: p.speed_mps, at: Date.now() });
  }, [nav?.position]);

  // The map's own proximity banner (cameras off the route) clears itself while driving.
  const proximityAlert = useAppStore((s) => s.proximityAlert);
  useEffect(() => {
    if (!active || !proximityAlert) return;
    const t = window.setTimeout(() => {
      const s = useAppStore.getState();
      if (s.proximityAlert === proximityAlert) {
        s.setProximityAlert(null);
        s.setHighlighted([]);
      }
    }, PROXIMITY_CLEAR_MS);
    return () => window.clearTimeout(t);
  }, [active, proximityAlert]);

  // Arrived: back to the map after 10 s (or a tap).
  const arrived = nav?.state === "arrived";
  useEffect(() => {
    if (!arrived) return;
    const t = window.setTimeout(() => void endNavigation(), ARRIVAL_CLOSE_MS);
    return () => window.clearTimeout(t);
  }, [arrived]);

  if (!nav) return null;
  const end = endNavigation;

  const toggleMute = () => {
    const muted = !nav.muted;
    useAppStore.getState().setNav({ ...nav, muted });
    void navApi.setMuted(muted);
  };

  if (arrived) {
    const sum = nav.summary;
    return (
      <div className="nav-arrived" role="dialog" aria-label="Arrived" onClick={() => void end()}>
        <div className="nav-arrived-card">
          <ManeuverIcon name="arrive" size={56} />
          <h2>You have arrived</h2>
          <div className="nav-dest">{nav.destination.label}</div>
          {sum && (
            <div className="nav-summary">
              <div>
                <strong>{navDuration(sum.elapsed_s)}</strong>
                <span>time</span>
              </div>
              <div>
                <strong>{navDistance(sum.distance_m)}</strong>
                <span>distance</span>
              </div>
              <div>
                <strong>{sum.cameras_passed}</strong>
                <span>camera{sum.cameras_passed === 1 ? "" : "s"} passed</span>
              </div>
            </div>
          )}
          <button className="btn primary nav-big">Done</button>
        </div>
      </div>
    );
  }

  const rerouting = nav.state === "off_route" || nav.state === "rerouting";
  const label = STATE_LABEL[nav.state];
  const alert = nav.camera_alert && nav.camera_alert_ms !== null && nav.now_ms - nav.camera_alert_ms < ALERT_SHOW_MS ? nav.camera_alert : null;
  const alertCam = alert?.cameras[0];

  return (
    <div className={`nav-view ${rerouting ? "rerouting" : ""}`}>
      <div className="nav-banner" role="status" aria-live="polite">
        {nav.acquiring ? (
          <div className="nav-step">
            <span className="nav-icon">
              <ManeuverIcon name="depart" size={56} />
            </span>
            <div className="nav-instr">Finding your location…</div>
          </div>
        ) : rerouting ? (
          <div className="nav-step">
            <span className="nav-icon spin">
              <ManeuverIcon name="uturn" size={56} />
            </span>
            <div className="grow">
              <div className="nav-dist">Rerouting…</div>
              <div className="nav-street">{nav.mode === "avoid" ? "Finding a way that still avoids cameras" : "Finding the fastest way from here"}</div>
            </div>
          </div>
        ) : nav.step ? (
          <Step step={nav.step} />
        ) : null}
        {nav.then && !rerouting && <Step step={nav.then} small />}
      </div>

      <div className="nav-chips">
        {label && !rerouting && <div className="nav-chip warn">{label}</div>}
        {nav.weak_signal && nav.state === "navigating" && <div className="nav-chip warn">Weak GPS signal</div>}
        {nav.notice && <div className="nav-chip warn">{nav.notice}</div>}
        {nav.camera_change && (
          <div className="nav-chip warn">
            The new route passes {nav.camera_change.after} camera{nav.camera_change.after === 1 ? "" : "s"} (the old one: {nav.camera_change.before}).
          </div>
        )}
        {nav.voice_unavailable && !voiceNoticeSeen && (
          <button className="nav-chip info" onClick={() => setVoiceNoticeSeen(true)}>
            Spoken directions aren't available on this phone (no US English text-to-speech voice). Guidance continues on screen. ×
          </button>
        )}
        {nav.throttled && <div className="nav-chip warn">Battery saver may pause GPS while the screen is off. Keep the screen on, or turn battery saver off.</div>}
        {nav.simulated && <div className="nav-chip info">Simulated drive</div>}
      </div>

      {alert && alertCam && (
        <div className={`nav-camera-alert ${alert.stage}`} role="alert">
          <span className="icon" aria-hidden="true">
            ⚠
          </span>
          <div className="grow">
            <div className="title">
              {alert.cameras.length > 1 ? `${alert.cameras.length} cameras` : alertCam.category === "flock" ? "Flock camera" : "ALPR camera"} in{" "}
              {navDistance(Math.max(0, (navRoute?.cameras.find((c) => c.key === alertCam.key)?.along_m ?? nav.progress_m + alertCam.distance_m) - nav.progress_m))}
            </div>
            <div className="sub">
              {alert.cameras.every((c) => c.source === "submission")
                ? "Unverified: your own submission"
                : alert.cameras.some((c) => c.source === "submission")
                  ? "Mapped in OpenStreetMap and unverified submissions"
                  : "Mapped in OpenStreetMap"}
              {alertCam.operator ? ` · ${alertCam.operator}` : ""}
            </div>
          </div>
        </div>
      )}

      <div className="nav-controls">
        {!follow && (
          <button className="btn nav-round primary" onClick={() => useAppStore.getState().setNavFollow(true)}>
            Recenter
          </button>
        )}
        <button
          className="btn nav-round"
          onClick={() => useAppStore.getState().setNavNorthUp(!northUp)}
          aria-pressed={northUp}
          title={northUp ? "North up (tap for heading up)" : "Heading up (tap for north up)"}
        >
          {northUp ? "N↑" : "⬆"}
        </button>
        <button className="btn nav-round" onClick={toggleMute} aria-pressed={nav.muted} title={nav.muted ? "Voice off" : "Voice on"}>
          {nav.muted ? "🔇" : "🔊"}
        </button>
      </div>

      <SimMenu nav={nav} />

      <div className="nav-bottom">
        <div className="grow nav-eta">
          <div className="nav-eta-time">{nav.eta_ms ? navClock(nav.eta_ms) : "--:--"}</div>
          <div className="nav-eta-rest">
            {navDuration(nav.remaining_s)} · {navDistance(nav.remaining_m)}
          </div>
        </div>
        <div className={`nav-cams ${nav.cameras_ahead === 0 ? "zero" : ""}`} title="Cameras still ahead on the route">
          <strong>{nav.cameras_ahead}</strong>
          <span>camera{nav.cameras_ahead === 1 ? "" : "s"} ahead</span>
        </div>
        <button className="btn danger nav-big" onClick={() => void end()}>
          End
        </button>
      </div>
    </div>
  );
}
