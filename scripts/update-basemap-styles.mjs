// Refresh the bundled OpenFreeMap style snapshots the map themes are built on.
//
//   node scripts/update-basemap-styles.mjs
//
// The themes (src/map/themes.ts) recolour these by layer role, so after an update run
// `npm test` — the theme tests check every theme still builds and passes the contrast check.
import { writeFile } from "node:fs/promises";

const STYLES = ["dark", "fiord", "positron"];

for (const name of STYLES) {
  const url = `https://tiles.openfreemap.org/styles/${name}`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  const style = await res.json();
  if (style.version !== 8 || !Array.isArray(style.layers)) throw new Error(`${url}: not a MapLibre style`);
  const out = new URL(`../src/map/styles/openfreemap-${name}.json`, import.meta.url);
  await writeFile(out, JSON.stringify(style, null, 1) + "\n");
  console.log(`${name}: ${style.layers.length} layers`);
}
