import { CATEGORY_LABELS, MARKER_COLORS, MARKER_HOLLOW, type MarkerKind } from "../lib/classify";
import { ALL_KINDS, sightingVisible } from "../lib/filters";
import { useAppStore } from "../store/useAppStore";

export default function FilterPanel() {
  const filters = useAppStore((s) => s.filters);
  const dispatch = useAppStore((s) => s.dispatchFilter);
  const inView = useAppStore((s) => s.inView);
  const sightings = useAppStore((s) => s.sightings);
  const setPanel = useAppStore((s) => s.setPanel);

  // Camera counts come from the same filtered index the map draws, for the current view.
  const totals: Record<MarkerKind, number> = { ...(inView?.totalByKind ?? { flock: 0, alpr: 0, user: 0 }), wifi: 0 };
  const visible: Record<MarkerKind, number> = { ...(inView?.byKind ?? { flock: 0, alpr: 0, user: 0 }), wifi: 0 };
  const sightingList = Object.values(sightings);
  totals.wifi = sightingList.length;
  visible.wifi = sightingList.filter((w) => sightingVisible(w, filters)).length;

  const kinds: MarkerKind[] = ALL_KINDS;
  return (
    <div className="panel">
      <div className="panel-header">
        <span>Filters</span>
        <button className="close" onClick={() => setPanel("none")} aria-label="Close">
          ×
        </button>
      </div>
      <div className="panel-body">
        <div className="section">
          <h4>Categories</h4>
          {kinds.map((k) => (
            <label key={k} className="checkbox">
              <input type="checkbox" checked={filters[k]} onChange={() => dispatch({ type: "toggle", kind: k })} />
              <span className={`swatch ${MARKER_HOLLOW[k] ? "hollow" : ""}`} style={{ background: MARKER_COLORS[k], color: MARKER_COLORS[k] }} />
              <span className="grow">{CATEGORY_LABELS[k]}</span>
              <span className="muted small">
                {visible[k]}/{totals[k]}
              </span>
              <button className="btn small" onClick={(e) => { e.preventDefault(); dispatch({ type: "solo", kind: k }); }} title="Show only this category">
                only
              </button>
            </label>
          ))}
          <div className="muted small" style={{ marginTop: 6 }}>
            Counts are for the current view; every cluster and density cell on the map is
            recomputed from the filtered set. Unverified cameras are your own submissions that
            are not in the synced OSM data yet: they count toward clusters and are marked
            separately (violet +n). Grey hollow rings (from zoom 14) are cameras missing from the
            latest sync, kept 30 days and not counted. Wi-Fi sightings are suspected devices
            inferred from Wi-Fi OUI matches (see Settings → Wi-Fi fingerprint dataset), not
            confirmed cameras, and load from zoom 11.
          </div>
        </div>
        <div className="section">
          <h4>Display</h4>
          <label className="checkbox">
            <input type="checkbox" checked={filters.cones} onChange={(e) => dispatch({ type: "cones", value: e.target.checked })} />
            <span className="grow">Direction cones</span>
          </label>
          <div className="muted small" style={{ marginTop: 4 }}>
            Shows which way a camera faces, from its OSM <code>direction</code> tag. Visible from zoom 13.
          </div>
        </div>
        <div className="section">
          <h4>Operator contains</h4>
          <input
            className="input"
            placeholder="e.g. police, Home Depot"
            value={filters.operator}
            onChange={(e) => dispatch({ type: "operator", text: e.target.value })}
          />
          <div className="muted small" style={{ marginTop: 4 }}>
            Matches the OSM <code>operator</code> tag (or the operator you entered on a submission).
            Cameras without an operator tag are hidden while this is set.
          </div>
        </div>
        <button className="btn small" onClick={() => dispatch({ type: "reset" })}>
          Reset filters
        </button>
      </div>
    </div>
  );
}
