import { useRef, useState } from "react";
import { placeSlots, SLOT_LABEL, type PlaceSlot } from "../lib/places";
import { navigateToPlace } from "../lib/quickNav";
import { useAppStore } from "../store/useAppStore";
import PlaceIcon from "./PlaceIcon";

function slotKey(s: PlaceSlot): string {
  return s.kind === "custom" ? `custom:${s.place.id}` : s.kind;
}

/**
 * One-tap trips to saved places, under the toolbar: Home, Work, then custom places. Scrolls
 * sideways when they don't fit. Not shown while turn-by-turn navigation runs (App renders the
 * navigation screen instead).
 */
export default function QuickNavBar() {
  const places = useAppStore((s) => s.savedPlaces);
  const openPlaces = useAppStore((s) => s.openPlaces);
  const [locating, setLocating] = useState<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  if (places === null) return null;

  const go = async (s: PlaceSlot) => {
    if (!s.place) {
      openPlaces({ slot: s.kind as "home" | "work" });
      return;
    }
    const key = slotKey(s);
    setLocating(key);
    try {
      await navigateToPlace(s.place);
    } finally {
      setLocating((k) => (k === key ? null : k));
    }
  };

  return (
    <div className="quick-nav" role="toolbar" aria-label="Saved places">
      <div
        ref={scroller}
        className="quick-nav-scroll"
        // A mouse wheel scrolls the row sideways.
        onWheel={(e) => {
          const el = scroller.current;
          if (el && Math.abs(e.deltaY) > Math.abs(e.deltaX)) el.scrollLeft += e.deltaY;
        }}
      >
        {places.length === 0 ? (
          <button className="qn-chip" onClick={() => openPlaces()} title="Save Home, Work and other places for one-tap directions">
            <span className="qn-pill add">+ Add place</span>
          </button>
        ) : (
          placeSlots(places).map((s) => {
            const key = slotKey(s);
            const label = s.kind === "custom" ? s.place.label : SLOT_LABEL[s.kind];
            const busy = locating === key;
            return (
              <button
                key={key}
                className="qn-chip"
                onClick={() => void go(s)}
                aria-busy={busy}
                title={s.place ? `Directions to ${label}: ${s.place.address}` : `Set your ${label} address`}
              >
                <span className={`qn-pill ${s.place ? "" : "unset"} ${busy ? "loading" : ""}`}>
                  {busy ? <span className="qn-spinner" aria-hidden="true" /> : <PlaceIcon kind={s.kind} />}
                  <span className="qn-label">{label}</span>
                  {busy && <span className="sr-only">Finding your location…</span>}
                </span>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}
