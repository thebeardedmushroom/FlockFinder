import { useEffect, useRef, useState } from "react";
import { reloadPlaces } from "../lib/actions";
import { api } from "../lib/ipc";
import { customLimitReached, labelError, MAX_CUSTOM_PLACES, MAX_LABEL_CHARS, slotPlace, SLOT_LABEL, suggestedLabel } from "../lib/places";
import { errorMessage, isAppError } from "../lib/types";
import { useAppStore } from "../store/useAppStore";
import Modal from "./Modal";
import PlaceIcon from "./PlaceIcon";

/**
 * "Save place" for a searched address or a dropped pin: set it as Home or Work (confirming a
 * replacement), or save it as a custom place under a name.
 */
export default function SavePlaceDialog() {
  const target = useAppStore((s) => s.savePlaceTarget);
  const close = () => useAppStore.getState().setSavePlaceTarget(null);
  const places = useAppStore((s) => s.savedPlaces) ?? [];
  const [address, setAddress] = useState("");
  const [label, setLabel] = useState("");
  /** The user typed a name: a late address lookup leaves it alone. */
  const labelEdited = useRef(false);
  const [looking, setLooking] = useState(false);
  const [replacing, setReplacing] = useState<"home" | "work" | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!target) return;
    setAddress(target.address);
    setLabel(target.name ? suggestedLabel(target.name, target.address) : target.source === "pin" ? "Dropped pin" : suggestedLabel(null, target.address));
    labelEdited.current = false;
    setReplacing(null);
    setError(null);
    if (target.source !== "pin") return;
    // A dropped pin only has coordinates: look up the street address for it.
    let live = true;
    setLooking(true);
    api
      .reverseGeocode(target.lat, target.lon)
      .then((found) => {
        if (!live || !found) return;
        setAddress(found);
        if (!labelEdited.current) setLabel(suggestedLabel(null, found));
      })
      .catch(() => {
        /* offline: keep the coordinates as the address */
      })
      .finally(() => live && setLooking(false));
    return () => {
      live = false;
    };
  }, [target]);

  if (!target) return null;
  const labelProblem = labelError(places, label);
  const atLimit = customLimitReached(places);

  const save = async (kind: "home" | "work" | "custom") => {
    if (kind !== "custom" && slotPlace(places, kind) && replacing !== kind) {
      setReplacing(kind);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const saved = await api.savePlace({ kind, label: kind === "custom" ? label.trim() : SLOT_LABEL[kind], address, lat: target.lat, lon: target.lon });
      await reloadPlaces();
      useAppStore.getState().pushToast(kind === "custom" ? `Saved "${saved.label}".` : `Set as ${saved.label}.`, "success");
      close();
    } catch (e) {
      setError(isAppError(e) && e.kind === "invalid" ? e.message.replace(/^invalid input:\s*/i, "").replace(/^./, (c) => c.toUpperCase()) : errorMessage(e));
      setReplacing(null);
    } finally {
      setSaving(false);
    }
  };

  const openSettings = () => {
    close();
    useAppStore.getState().select(null);
    useAppStore.getState().openPlaces();
  };

  if (replacing) {
    const current = slotPlace(places, replacing)!;
    const name = SLOT_LABEL[replacing];
    return (
      <Modal
        title={`Replace ${name}?`}
        onClose={close}
        footer={
          <>
            <button className="btn" onClick={() => setReplacing(null)} disabled={saving}>
              Back
            </button>
            <button className="btn primary" onClick={() => void save(replacing)} disabled={saving}>
              {saving ? "Saving…" : `Replace ${name}`}
            </button>
          </>
        }
      >
        <p>
          {name} is set to <strong>{current.address}</strong>.
        </p>
        <p>
          Replace it with <strong>{address}</strong>?
        </p>
        {error && <div className="callout error" role="alert">{error}</div>}
      </Modal>
    );
  }

  return (
    <Modal title="Save place" onClose={close} width={440}>
      <div className="save-place-address">
        <PlaceIcon kind="custom" filled={false} />
        <span className="grow">
          {address}
          {looking && <span className="muted small"> · finding the address…</span>}
        </span>
      </div>
      <div className="save-place-slots">
        {(["home", "work"] as const).map((k) => {
          const current = slotPlace(places, k);
          return (
            <button key={k} className="btn save-slot" onClick={() => void save(k)} disabled={saving}>
              <PlaceIcon kind={k} />
              <span className="grow">
                Set as {SLOT_LABEL[k]}
                {current && <span className="muted small ellipsis">Replaces {current.address}</span>}
              </span>
            </button>
          );
        })}
      </div>
      <div className="field">
        <label htmlFor="save-place-label">Or save it as</label>
        <div className="row">
          <input
            id="save-place-label"
            className={`input grow ${labelProblem && label.trim() ? "invalid" : ""}`}
            value={label}
            maxLength={MAX_LABEL_CHARS}
            disabled={atLimit}
            aria-invalid={!!labelProblem}
            onChange={(e) => {
              setLabel(e.target.value);
              labelEdited.current = true;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !labelProblem && !atLimit) void save("custom");
            }}
          />
          <button className="btn primary" onClick={() => void save("custom")} disabled={saving || atLimit || !!labelProblem}>
            ★ Save
          </button>
        </div>
        {labelProblem && !atLimit && <span className="field-error">{labelProblem}</span>}
      </div>
      {atLimit && (
        <div className="callout warn">
          You've saved {MAX_CUSTOM_PLACES} places besides Home and Work, the most allowed. Delete one in Saved places to save this one.
          <div className="row">
            <button className="btn small" onClick={openSettings}>
              Open Saved places
            </button>
          </div>
        </div>
      )}
      {error && <div className="callout error" role="alert">{error}</div>}
    </Modal>
  );
}
