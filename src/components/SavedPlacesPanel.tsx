import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { reloadPlaces, toastError } from "../lib/actions";
import { api } from "../lib/ipc";
import {
  customLimitReached,
  customPlaces,
  labelError,
  MAX_CUSTOM_PLACES,
  MAX_LABEL_CHARS,
  moveItem,
  slotPlace,
  SLOT_LABEL,
  suggestedLabel,
} from "../lib/places";
import { errorMessage, isAppError, type GeocodeResult, type PlaceKind, type SavedPlace } from "../lib/types";
import { useAppStore, type PlacesFocus } from "../store/useAppStore";
import PlaceIcon from "./PlaceIcon";
import { useSheet } from "./useSheet";

/** The place being added or edited. */
interface Draft {
  kind: PlaceKind;
  /** Set when editing an existing custom place. */
  id: number | null;
  label: string;
  /** The chosen geocoding result; saving needs one. */
  picked: { address: string; lat: number; lon: number } | null;
}

function draftFor(focus: PlacesFocus | null, places: SavedPlace[]): Draft | null {
  if (!focus) return null;
  if (focus.slot === "new") return { kind: "custom", id: null, label: "", picked: null };
  if (focus.slot === "custom") {
    const p = places.find((x) => x.id === focus.id);
    return p ? { kind: "custom", id: p.id, label: p.label, picked: { address: p.address, lat: p.lat, lon: p.lon } } : null;
  }
  const p = slotPlace(places, focus.slot);
  return { kind: focus.slot, id: p?.id ?? null, label: SLOT_LABEL[focus.slot], picked: p ? { address: p.address, lat: p.lat, lon: p.lon } : null };
}

function geocodeErrorText(e: unknown): string {
  if (isAppError(e) && e.kind === "offline") return "You're offline, so addresses can't be searched right now. Saved places still work; try again when you're connected.";
  if (isAppError(e) && e.kind === "rate_limited") return "The address search (Nominatim) is busy. Wait a moment and try again.";
  return `Address search failed: ${errorMessage(e)}`;
}

function PlaceEditor({ draft: initial, places, onClose }: { draft: Draft; places: SavedPlace[]; onClose: () => void }) {
  const pushToast = useAppStore((s) => s.pushToast);
  const [draft, setDraft] = useState(initial);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<GeocodeResult[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [touched, setTouched] = useState(false);
  const queryRef = useRef<HTMLInputElement>(null);
  const custom = draft.kind === "custom";
  const labelProblem = custom ? labelError(places, draft.label, draft.id) : null;
  const atLimit = custom && draft.id === null && customLimitReached(places);

  useEffect(() => {
    queryRef.current?.focus();
  }, []);

  const search = async () => {
    const q = query.trim();
    if (!q || searching) return;
    setSearching(true);
    setSearchError(null);
    try {
      const r = await api.geocode(q);
      setResults(r);
      if (r.length === 0) setSearchError(`No places found for "${q}". Try a fuller address.`);
    } catch (e) {
      setResults(null);
      setSearchError(geocodeErrorText(e));
      if (isAppError(e) && e.kind === "offline") useAppStore.getState().setOffline(true);
    } finally {
      setSearching(false);
    }
  };

  const pick = (r: GeocodeResult) => {
    setDraft((d) => ({
      ...d,
      picked: { address: r.display_name, lat: r.lat, lon: r.lon },
      // A new custom place takes its name from the result unless one was typed.
      label: d.kind === "custom" && !d.label.trim() ? suggestedLabel(null, r.display_name) : d.label,
    }));
    setResults(null);
    setQuery("");
    setSearchError(null);
  };

  const save = async () => {
    setTouched(true);
    if (!draft.picked || labelProblem || atLimit) return;
    setSaving(true);
    setSaveError(null);
    try {
      await api.savePlace({ id: draft.id, kind: draft.kind, label: draft.label.trim(), ...draft.picked });
      await reloadPlaces();
      pushToast(custom ? `Saved "${draft.label.trim()}".` : `${SLOT_LABEL[draft.kind as "home" | "work"]} saved.`, "success");
      onClose();
    } catch (e) {
      setSaveError(isAppError(e) && e.kind === "invalid" ? e.message.replace(/^invalid input:\s*/i, "").replace(/^./, (c) => c.toUpperCase()) : errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  const title = custom ? (draft.id === null ? "Add a place" : `Edit ${initial.label}`) : `${draft.id === null ? "Set" : "Change"} ${SLOT_LABEL[draft.kind as "home" | "work"]}`;
  return (
    <div className="section place-editor" aria-label={title}>
      <h4>{title}</h4>
      {atLimit && (
        <div className="callout warn">You've saved {MAX_CUSTOM_PLACES} places besides Home and Work, the most allowed. Delete one to add another.</div>
      )}
      <div className="field">
        <label htmlFor="place-label">Name</label>
        {custom ? (
          <>
            <input
              id="place-label"
              className={`input ${touched && labelProblem ? "invalid" : ""}`}
              value={draft.label}
              maxLength={MAX_LABEL_CHARS}
              placeholder="e.g. Gym, Mom's, School"
              aria-invalid={touched && !!labelProblem}
              onChange={(e) => {
                setDraft({ ...draft, label: e.target.value });
                setTouched(true);
              }}
            />
            {touched && labelProblem && <span className="field-error">{labelProblem}</span>}
          </>
        ) : (
          <div className="row place-fixed-label">
            <PlaceIcon kind={draft.kind} /> {SLOT_LABEL[draft.kind as "home" | "work"]}
          </div>
        )}
      </div>
      <div className="field">
        <label htmlFor="place-address">Address</label>
        {draft.picked ? (
          <div className="place-picked">
            <span className="grow">{draft.picked.address}</span>
          </div>
        ) : (
          <span className="muted small">Search for the address, then choose it from the list.</span>
        )}
        <div className="row">
          <input
            id="place-address"
            ref={queryRef}
            className="input grow"
            value={query}
            autoComplete="off"
            placeholder={searching ? "Searching…" : draft.picked ? "Search for a different address" : "Street address or place"}
            onChange={(e) => {
              setQuery(e.target.value);
              setResults(null);
              setSearchError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void search();
            }}
          />
          <button className="btn small" onClick={() => void search()} disabled={!query.trim() || searching}>
            {searching ? "…" : "Search"}
          </button>
        </div>
        {searchError && <span className={`small ${searchError.startsWith("No places") ? "muted" : "field-error"}`}>{searchError}</span>}
        {results && results.length > 0 && (
          <div className="choices" role="listbox" aria-label="Matching addresses">
            <span className="muted small">Choose the address:</span>
            {results.map((r, i) => (
              <button key={i} role="option" aria-selected={false} onClick={() => pick(r)}>
                {r.display_name}
              </button>
            ))}
          </div>
        )}
        {touched && !draft.picked && <span className="field-error">Choose an address from the search results to save this place.</span>}
      </div>
      {saveError && <div className="callout error" role="alert">{saveError}</div>}
      <div className="row">
        <button className="btn primary" onClick={() => void save()} disabled={saving || atLimit || (touched && (!draft.picked || !!labelProblem))}>
          {saving ? "Saving…" : "Save"}
        </button>
        <button className="btn" onClick={onClose} disabled={saving}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/** Delete or clear, after a confirmation. */
function ConfirmRemove({ place, onDone }: { place: SavedPlace; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const slot = place.kind !== "custom";
  const remove = async () => {
    setBusy(true);
    try {
      await api.deletePlace(place.id);
      await reloadPlaces();
      useAppStore.getState().pushToast(slot ? `${place.label} cleared.` : `"${place.label}" deleted.`, "success");
    } catch (e) {
      toastError(e, slot ? "Could not clear it" : "Could not delete it");
    } finally {
      setBusy(false);
      onDone();
    }
  };
  return (
    <div className="callout warn place-confirm" role="alertdialog" aria-label={slot ? `Clear ${place.label}?` : `Delete ${place.label}?`}>
      {slot ? `Clear your ${place.label} address (${place.address})?` : `Delete "${place.label}"? This can't be undone.`}
      <div className="row">
        <button className="btn small danger" onClick={() => void remove()} disabled={busy}>
          {slot ? "Clear" : "Delete"}
        </button>
        <button className="btn small" onClick={onDone} disabled={busy}>
          Keep
        </button>
      </div>
    </div>
  );
}

interface DragState {
  pointer: number;
  from: number;
  over: number;
  /** Pointer travel since the press. */
  dy: number;
  y0: number;
  /** Row midpoints when the drag started. */
  mids: number[];
  /** How far the other rows move to make room (the dragged row's height plus the gap). */
  step: number;
}

export default function SavedPlacesPanel() {
  const places = useAppStore((s) => s.savedPlaces) ?? [];
  const focus = useAppStore((s) => s.placesFocus);
  const setPanel = useAppStore((s) => s.setPanel);
  const sheet = useSheet(focus);
  const [draft, setDraft] = useState<Draft | null>(() => draftFor(focus, places));
  const [confirm, setConfirm] = useState<number | null>(null);
  const [drag, setDrag] = useState<DragState | null>(null);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);

  // Opened again on another slot (an unset chip, "Edit saved place").
  useEffect(() => {
    setDraft(draftFor(focus, useAppStore.getState().savedPlaces ?? []));
    setConfirm(null);
  }, [focus]);

  const customs = customPlaces(places);
  // While dragging, rows only move visually (the DOM order stays put, so the pointer stays captured).
  const offset = (i: number): number => {
    if (!drag) return 0;
    const { from, over, dy, step } = drag;
    if (i === from) return dy;
    if (from < over && i > from && i <= over) return -step;
    if (from > over && i >= over && i < from) return step;
    return 0;
  };

  const commitOrder = async (ordered: SavedPlace[]) => {
    const store = useAppStore.getState();
    // Shown at once; the stored order follows.
    const bySlot = (store.savedPlaces ?? []).filter((p) => p.kind !== "custom");
    store.setSavedPlaces([...bySlot, ...ordered.map((p, i) => ({ ...p, sort_order: i }))]);
    try {
      store.setSavedPlaces(await api.reorderPlaces(ordered.map((p) => p.id)));
    } catch (e) {
      toastError(e, "Could not reorder");
      await reloadPlaces();
    }
  };

  const onHandleDown = (e: ReactPointerEvent<HTMLButtonElement>, index: number) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    setConfirm(null);
    const rects = customs.map((_, i) => rowRefs.current[i]?.getBoundingClientRect() ?? null);
    const mids = rects.map((r) => (r ? r.top + r.height / 2 : 0));
    const here = rects[index];
    const next = rects[index + 1] ?? null;
    const prev = rects[index - 1] ?? null;
    const step = here ? (next ? next.top - here.top : prev ? here.top - prev.top : here.height) : 0;
    setDrag({ pointer: e.pointerId, from: index, over: index, dy: 0, y0: e.clientY, mids, step });
  };
  const onHandleMove = (e: ReactPointerEvent<HTMLButtonElement>) => {
    if (!drag || e.pointerId !== drag.pointer) return;
    const dy = e.clientY - drag.y0;
    const center = drag.mids[drag.from] + dy;
    const over = drag.mids.filter((m, i) => i !== drag.from && m < center).length;
    setDrag({ ...drag, dy, over });
  };
  const onHandleUp = (e: ReactPointerEvent<HTMLButtonElement>) => {
    if (!drag || e.pointerId !== drag.pointer) return;
    const { from, over } = drag;
    setDrag(null);
    if (from !== over) void commitOrder(moveItem(customs, from, over));
  };
  const onHandleKey = (e: ReactKeyboardEvent<HTMLButtonElement>, index: number) => {
    const to = e.key === "ArrowUp" ? index - 1 : e.key === "ArrowDown" ? index + 1 : null;
    if (to === null || to < 0 || to >= customs.length) return;
    e.preventDefault();
    void commitOrder(moveItem(customs, index, to));
    // Keep the focus on the handle of the row that moved.
    window.setTimeout(() => rowRefs.current[to]?.querySelector<HTMLButtonElement>(".drag-handle")?.focus(), 0);
  };

  const editing = (p: SavedPlace | null, slot: "home" | "work" | null) =>
    draft !== null && (slot ? draft.kind === slot : draft.kind === "custom" && draft.id === p?.id);

  const slotRow = (slot: "home" | "work") => {
    const p = slotPlace(places, slot);
    return (
      <div key={slot} className={`place-row ${p ? "" : "unset"} ${editing(p, slot) ? "selected" : ""}`}>
        <span className="place-row-icon">
          <PlaceIcon kind={slot} />
        </span>
        <span className="grow place-row-text">
          <span className="place-row-label">{SLOT_LABEL[slot]}</span>
          <span className="muted small ellipsis">{p ? p.address : "Not set"}</span>
        </span>
        <button className="btn small" onClick={() => setDraft(draftFor({ slot }, places))}>
          {p ? "Edit" : "Set"}
        </button>
        {p && (
          <button className="btn small danger" onClick={() => setConfirm(p.id)} aria-label={`Clear ${SLOT_LABEL[slot]}`}>
            Clear
          </button>
        )}
      </div>
    );
  };

  return (
    <div ref={sheet.ref} className={`panel saved-places ${sheet.className}`}>
      <div className="panel-header" {...sheet.headerProps}>
        {sheet.grip}
        <span>Saved places</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        {draft && <PlaceEditor key={`${draft.kind}:${draft.id ?? "new"}`} draft={draft} places={places} onClose={() => setDraft(null)} />}

        <div className="section">
          <div className="place-list">
            {slotRow("home")}
            {confirm !== null && places.find((p) => p.id === confirm && p.kind === "home") && (
              <ConfirmRemove place={places.find((p) => p.id === confirm)!} onDone={() => setConfirm(null)} />
            )}
            {slotRow("work")}
            {confirm !== null && places.find((p) => p.id === confirm && p.kind === "work") && (
              <ConfirmRemove place={places.find((p) => p.id === confirm)!} onDone={() => setConfirm(null)} />
            )}
          </div>
        </div>

        <div className="section">
          <h4>
            Other places ({customs.length} of {MAX_CUSTOM_PLACES})
          </h4>
          {customs.length === 0 ? (
            <p className="muted small">None yet. Add one here, or search an address on the map and choose ☆ Save place.</p>
          ) : (
            <div className="place-list" onPointerCancel={() => setDrag(null)}>
              {customs.map((p, i) => (
                <div
                  key={p.id}
                  ref={(el) => {
                    rowRefs.current[i] = el;
                  }}
                  className={`place-item ${drag?.from === i ? "dragging" : ""} ${drag ? "drag-active" : ""}`}
                  style={drag ? { transform: `translateY(${offset(i)}px)` } : undefined}
                >
                  <div className={`place-row ${editing(p, null) ? "selected" : ""}`}>
                    <button
                      className="drag-handle"
                      aria-label={`Move ${p.label}. Drag, or use the up and down arrow keys.`}
                      title="Drag to reorder"
                      onPointerDown={(e) => onHandleDown(e, i)}
                      onPointerMove={onHandleMove}
                      onPointerUp={onHandleUp}
                      onPointerCancel={() => setDrag(null)}
                      onKeyDown={(e) => onHandleKey(e, i)}
                    >
                      <svg width="12" height="18" viewBox="0 0 12 18" aria-hidden="true" fill="currentColor">
                        {[3, 9, 15].map((y) => (
                          <g key={y}>
                            <circle cx="3" cy={y} r="1.6" />
                            <circle cx="9" cy={y} r="1.6" />
                          </g>
                        ))}
                      </svg>
                    </button>
                    <span className="place-row-icon">
                      <PlaceIcon kind="custom" />
                    </span>
                    <span className="grow place-row-text">
                      <span className="place-row-label ellipsis">{p.label}</span>
                      <span className="muted small ellipsis">{p.address}</span>
                    </span>
                    <button className="btn small" onClick={() => setDraft(draftFor({ slot: "custom", id: p.id }, places))}>
                      Edit
                    </button>
                    <button className="btn small danger" onClick={() => setConfirm(p.id)} aria-label={`Delete ${p.label}`}>
                      Delete
                    </button>
                  </div>
                  {confirm === p.id && <ConfirmRemove place={p} onDone={() => setConfirm(null)} />}
                </div>
              ))}
            </div>
          )}
          <div className="row" style={{ marginTop: 8 }}>
            <button className="btn small" onClick={() => setDraft(draftFor({ slot: "new" }, places))} disabled={customLimitReached(places)}>
              + Add place
            </button>
          </div>
          {customLimitReached(places) && (
            <p className="muted small">You've saved {MAX_CUSTOM_PLACES} places besides Home and Work, the most allowed. Delete one to add another.</p>
          )}
        </div>

        <p className="muted small">
          Saved places stay on this device; they aren't synced between devices. Tap one in the bar at the top of the map
          for directions from where you are. Addresses are looked up with Nominatim when you search for them; trips
          use the location saved here, so the address isn't looked up again.
        </p>
      </div>
    </div>
  );
}
