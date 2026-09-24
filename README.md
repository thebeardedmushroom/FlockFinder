# Flock Finder

A cross-platform Tauri v2 app (Windows, macOS, Linux and Android) that maps automated license plate reader (ALPR)
cameras — Flock Safety and other vendors — from crowdsourced **OpenStreetMap** data. It
shows an interactive map, lets you record cameras you have personally observed, and tells
you when new cameras appear inside places or along routes you save.

> **Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence
> of a camera.** OSM only contains what volunteers have surveyed. An empty map means
> nobody has mapped that area yet.

The app maps fixed hardware only. It never handles license plate data, vehicle data, or
information about individuals. Your own location is used only while the locate button is on,
to show where you are and (optionally) to warn you when you come near a mapped camera; it is
never stored or uploaded. Watch-area and route alerts are computed against places and routes
you explicitly save, not your live position.

---

## Setup

Prerequisites (see the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/)
for your platform):

- Rust (stable, 1.80+) and Cargo
- Node.js 20+ and npm
- Linux: `libwebkit2gtk-4.1-dev`, `build-essential`, `libssl-dev`, `libxdo-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev`
- Windows: WebView2 (preinstalled on Windows 10/11) and the MSVC build tools
- macOS: Xcode command-line tools

```bash
npm install
npm run tauri dev      # develop (Vite on http://localhost:14210 + Rust backend)
npm run tauri build    # produce installers under src-tauri/target/release/bundle/
```

Tests:

```bash
npm test                          # frontend: classification + filter state machine (vitest)
cd src-tauri && cargo test        # Rust: grid/bbox math, haversine, point-to-polyline,
                                  # GPX parsing, Overpass parsing (incl. malformed input),
                                  # cache/stale logic, alert diffing, JOSM export, PKCE
```

Before distributing a build, set `repository` in `src-tauri/Cargo.toml` to your real
GitHub repository URL. It is embedded in every outbound `User-Agent` header
(`FlockFinder/<version> (+<repository>)`) so the Overpass and Nominatim operators can
contact you, and it is where the app looks for the daily camera snapshot
(`<repository>/releases/download/camera-snapshot/manifest.json`). While it is still the
`OWNER` placeholder, the app queries Overpass directly.

### GitHub workflows

- `.github/workflows/ci.yml`: type-check, frontend tests and `cargo test` on every push
  to `main` and every pull request.
- `.github/workflows/camera-snapshot.yml`: runs the worldwide Overpass query once a day
  (04:17 UTC, or on demand from the Actions tab), validates the answer and publishes it to
  the rolling `camera-snapshot` release. It needs no secrets; the repository's
  *Settings → Actions → General → Workflow permissions* must allow read and write. Run it
  once by hand after creating the repository so installs have a snapshot to download.

### Working offline / without network

- `FLOCKFINDER_OFFLINE_FIXTURE=1 npm run tauri dev` makes the camera sync ingest the bundled
  Overpass sample (`src-tauri/fixtures/overpass_sample.json`, 453 real elements from the
  Denver metro, fetched 2026-09-10) instead of calling Overpass.
- `FLOCKFINDER_SYNC_FILE=<path>` makes the sync ingest a saved worldwide Overpass response
  from disk (development: exercise the full-size pipeline without another global query).
- Settings → Camera data → **Load sample data** ingests the fixture into the local store and
  flies the map to it.
- `npm run dev`, then `http://localhost:14210/?mock=1`, runs the UI in a browser against an
  in-memory mock backend. It serves `dev-data/overpass_global.json` (a saved worldwide
  response; gitignored because it is ODbL data) when present, else the fixture. Add
  `&sync=never`, `&sync=failed` or `&sync=syncing` to start in that sync state.

### Android (API 37)

The same app builds for Android 17: `compileSdk`/`targetSdk` 37, `minSdk` 24. One-time setup
on top of the desktop prerequisites:

- A JDK 17 or 21. Android Studio's bundled JDK is 25, which is too new for the Gradle 8.14 the
  Tauri template uses ("Unsupported class file major version 69").
- In the Android SDK: platform `android-37.0`, build-tools `37.0.0`, NDK `30.0.16248370`,
  command-line tools
- `rustup target add aarch64-linux-android x86_64-linux-android`
- Windows: turn on **Developer Mode** (Settings → System → For developers). The Tauri CLI
  symlinks the compiled library into the Android project, which Windows only allows there.

Per shell (PowerShell shown):

```powershell
$env:JAVA_HOME = "C:\path\to\jdk-21"   # JDK 17-21, not Android Studio's JDK 25
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME = "$env:ANDROID_HOME\ndk\30.0.16248370"
```

```bash
npm run tauri android dev                                     # emulator or USB device
npm run tauri android build -- --apk --target aarch64 x86_64  # installable APKs
```

- The generated Android Studio project lives in `src-tauri/gen/android` and is committed.
  `compileSdk 37` is newer than the Android Gradle Plugin (8.11) the Tauri template ships, so
  `gradle.properties` sets `android.suppressUnsupportedCompileSdk=37`; AGP 9.4+ supports 37
  natively.
- Differences from desktop: TLS uses rustls with Mozilla's bundled root certificates; the OSM
  token is kept in Shared Preferences encrypted with the Android Keystore; files are picked
  through Android's document picker; long-press the map for the context menu; the
  notification permission is requested when you save your first watch area or route; and
  scheduled alert refreshes only run while the app is open (there is no background service).
- Release signing is not configured yet; see Tauri's
  [Android signing guide](https://v2.tauri.app/distribute/sign/android/).

### Data location

SQLite database in the platform app-data directory (shown under Settings → About):

| Platform | Path |
|---|---|
| Linux | `~/.local/share/org.flockfinder.app/flockfinder.sqlite` |
| Windows | `%APPDATA%\org.flockfinder.app\flockfinder.sqlite` |
| macOS | `~/Library/Application Support/org.flockfinder.app/flockfinder.sqlite` |

Schema changes ship as numbered files in `src-tauri/migrations/`; the schema is never
mutated in place.

---

## How it works

### Data source and classification

Every camera in the world comes from one worldwide Overpass request (the **camera sync**).
The query lives in `src-tauri/data/global_query.overpassql`:

```overpassql
[out:json][timeout:600];
(
  node["man_made"="surveillance"]["surveillance:type"="ALPR"];
  way["man_made"="surveillance"]["surveillance:type"="ALPR"];
  node["man_made"="surveillance"]["surveillance:zone"="traffic"]["camera:type"="fixed"]["brand"~"[Ff]lock"];
);
out center tags;
```

Measured on 2026-09-11 against overpass-api.de: 150,947 elements, 3.9 MB on the wire
(gzip), 56.8 MB of JSON, about 200 s of server time. The result goes into the local SQLite
store in one transaction, and the map, proximity alerts and route reports all read from that
store. Moving the map makes no network requests at any zoom.

Installs don't send this query themselves. The `camera-snapshot` workflow runs it once a day
and publishes the answer, gzipped, as a GitHub release asset alongside a `manifest.json`
(generation time, OSM timestamp, element count, size, SHA-256). The app:

1. fetches the manifest once a day (a few hundred bytes);
2. stops if the snapshot is no newer than the data it already applied;
3. otherwise downloads the file (about 4 MB), checks its size and checksum against the
   manifest, and ingests it through the same parser and plausibility checks as an
   Overpass answer.

It queries Overpass directly (at the user's sync interval, never daily) only when no
snapshot is published, the manifest is in a format it doesn't understand, or the snapshot
is more than 14 days old. It also does so when Settings → *Camera sync source* is set to
*Query Overpass directly*. Settings shows which source the stored data came from and the
OSM time it reflects.

Each result is classified into exactly one category:

| Category | Marker | Rule |
|---|---|---|
| Flock (confirmed) | filled disc, accent colour | any of `brand`, `manufacturer`, `operator` matches `/flock/i` |
| ALPR (vendor unknown) | ring, accent colour | `surveillance:type=ALPR` present, no Flock match |
| Unverified (user-submitted) | violet disc, white ring | recorded in this app, not (yet) in the synced OSM data |

A grey **hollow** ring (from zoom 14) is a camera that was in the store but missing from the
latest sync; it is kept for 30 days in case it was a transient edit, then deleted. Hollow
cameras are not counted anywhere.

From zoom 13, a camera with a `direction` (or `camera:direction`) tag gets a cone showing
which way it faces, as on DeFlock's map. Degrees, cardinal letters (`NE`), `;`-separated
lists (one cone each, up to six) and clockwise ranges (`338-23`, drawn at their real width)
are understood; a single bearing is drawn 45° wide. Submissions with a direction get one too.
Toggle under Filters → Display.

### Map levels of detail

Cameras are drawn at every zoom level; what changes is the representation. Breakpoints and
sizes live in `src/lib/lod.ts`.

| Band | Zoom | Representation |
|---|---|---|
| Wide | 0–5 | Hexagonal density field: cell fill brightness ∝ log(count), under the basemap's place labels. Cells wrap at the antimeridian. Basemap labels show Latin names only (see below). |
| Mid | 6–10 | Cluster nodes: **area** (not radius) ∝ count, brightness ∝ log density, exact count label (abbreviated only from 1,000: `1.2k`). |
| Near | 11–13 | Small clusters; isolated cameras drawn individually; direction cones from 13. |
| Detail | 14+ | Every camera individually, with direction cones. Co-located cameras spread out (spiderfy) when clicked. |

- Aggregation runs in a Web Worker: one Supercluster hierarchy and one set of hex bins per
  data or filter change, then only queries while the map moves. Every count respects the
  active filters. Clicking a cluster zooms to the bounds of its cameras.
- Unverified submissions count toward clusters by default and are marked, never silent: a
  violet ring and a violet `+n` after the verified count. The **Unverified** filter removes
  them.
- A legend is shown in the wide and mid bands; a HUD in the lower-left corner shows cameras
  in view, the total in the store and the last sync time.
- Level and band changes animate (clusters split out of / merge into their parent) in
  250 ms. With `prefers-reduced-motion` they crossfade in place, and the load sweep and the
  slow pulse on the densest clusters are off. There is no continuous animation.
- An empty view says which of three things is true: no cameras are mapped there, the data
  has never been synced, or the latest sync failed.
- The density field is drawn by the app's own WebGL layer from one static buffer per level,
  not a GeoJSON source, so panning creates no tiles for it.
- Basemap glyphs: Chinese, Japanese and Korean characters are fetched from the style's glyph
  server (`localIdeographFontFamily: false`) instead of being rasterised on the main thread.
  MapLibre still draws multi-codepoint grapheme clusters (Mongolian, Tibetan, Devanagari,
  Thai…) itself, on the main thread, with no option to avoid it; a wide-band pan streams whole
  continents of such labels past at once and stalled 25–50 ms per batch. So below zoom 6
  bilingual basemap labels show their Latin name only; from zoom 6 they are bilingual again.
  A custom style whose glyph server lacks CJK ranges would show those characters as missing.

### Sync, caching and etiquette

- A daily snapshot check (see above), or, when querying Overpass directly, one worldwide
  request per sync interval (default 7 days, 1–30 in Settings), shortly after launch when
  due. A failed or offline attempt is retried after an hour, not every minute.
  "Sync now" (toolbar and Settings) runs one on demand. Progress is shown while it runs.
- A worldwide response with fewer than half the cameras already stored is treated as
  incomplete and not applied, so a degraded answer can never hollow out the map. Overpass
  responses carrying a `runtime error` remark (timeouts, partial data) are rejected.
- The sync is committed in a single SQLite transaction. The map's compact camera snapshot is
  cached on disk and rebuilt only when a camera changed.
- Watch-area and route alerts still re-query just the 0.05° grid cells their targets touch
  (stamped with the same interval as a TTL), and a completed sync re-checks every target
  against the new data without further requests.
- Wi-Fi fingerprint sightings are a local table and load per viewport from zoom 11.
- All requests go through Rust with `User-Agent: FlockFinder/<version> (+<repo>)`.
- HTTP 429 / 504 are retried with 2 s, 8 s, 30 s backoff, never more than three times,
  then surfaced as a toast.
- Nominatim (place search, and the open-water check when saving a submission) is limited
  to one request per second and results are cached by query string.
- Basemap tiles are the one thing loaded by the webview directly (from the configured
  MapLibre style URL; default OpenFreeMap `dark`, verified live on 2026-09-10; `fiord`,
  `liberty`, `positron` and `bright` are keyless alternatives). Change or blank it under
  Settings. Migration 0003 moves installs that were on the earlier `liberty` default to
  `dark`; custom URLs are left alone.

### Submissions and the OSM tagging scheme

"Add camera" drops a draggable pin; the form records category, the direction the camera
faces, mount type, operator and private notes. Submissions are stored locally with status
`local`. Notes never leave the machine. Before saving, the app warns about another
submission or a mapped OSM camera within 15 m, and blocks pins in open water (via Nominatim
reverse geocoding; skipped when offline).

Tags written to OSM (via JOSM export or direct upload):

| Tag | Value |
|---|---|
| `man_made` | `surveillance` |
| `surveillance` | `public` |
| `surveillance:type` | `ALPR` |
| `surveillance:zone` | `traffic` |
| `camera:type` | `fixed` |
| `brand`, `manufacturer` | `Flock Safety` (only when the category is Flock) |
| `camera:mount` | `pole`, `mast` or `wall` (from pole / mast / building) when set |
| `direction` | 0–359 when set |
| `operator` | free text when set |

Two ways to contribute:

1. **JOSM export (no account needed in the app).** Submissions → *Export all local as
   .osm* writes a JOSM-compatible file with negative ids. Open it in JOSM, review, upload.
2. **Direct upload (optional).** Register an OAuth 2 application at
   <https://www.openstreetmap.org/oauth2/applications> with redirect URI
   `flockfinder://oauth/callback`, scope `write_api`, **not** confidential (public client,
   PKCE). Paste the client ID into Settings and sign in. The token is stored in the OS
   keychain, never in SQLite. Every upload is one camera in one changeset
   (`created_by=FlockFinder/<version>`, plus your required comment) and must pass a
   preflight dialog that shows the exact tags and requires confirming you personally
   observed the camera. Failures are shown verbatim and never retried automatically. If
   your session has expired the app asks you to sign in again. If the browser cannot hand
   the callback back to the app, paste the `flockfinder://…` URL into Settings.

Deleting an uploaded submission removes only the local copy; the OSM element stays.

### Wi-Fi fingerprint sightings (optional second source)

Flock Safety cameras carry Wi-Fi radios whose MAC prefixes (OUIs) have been catalogued by
the community. The separate open-source project
[simeononsecurity/flock-finder](https://github.com/simeononsecurity/flock-finder) (MIT;
it shares this app's name but is otherwise unrelated) queries the crowdsourced
[WiGLE](https://wigle.net) wardriving database for those 31 prefixes and publishes the
result daily as CSV/GeoJSON. This app can:

- **Download that dataset** (Settings → Wi-Fi fingerprint dataset, ≈24 MB, opt-in) into
  a local `wifi_sightings` table, applying the same 730-day retention and keep-latest
  deduplication as upstream.
- **Import your own Wigle-format CSV** files (WiGLE app exports, ESP32 Marauder
  "Flock Wardrive" output, etc.). Only rows matching a bundled OUI or a Flock-like SSID
  (`Flock`, `Flock-1A2B3C`, `Flock001`, `Flock Camera net.`) are kept; nothing is uploaded.

The same pipeline is available headless for scripting (a weekly task, cron, CI):

```bash
cd src-tauri && cargo run --release --bin flockfinder-wifi-sync
# options: --db <path>  --csv <local flock_cameras.csv>  --clear  --view lat,lon,zoom
```

It writes to the app's own database (safe while the app is open; the map re-queries on
its next move or when the window regains focus).

Sightings render as a distinct sky-blue layer with its own filter toggle and detail panel.
They are **suspected, unconfirmed** devices: an OUI match is a heuristic, positions are
where a passing scanner heard the radio, and records can be stale. They are kept strictly
apart from OSM cameras, never feed alerts, and cannot be uploaded to OSM. The OUI list
lives in `src-tauri/data/flock_ouis.json`; credits and license text are in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

### Your location

The locate button (bottom right, above the zoom buttons) uses the WebView's Geolocation API.
The first press centres the map on you (zoom 15 at most); while it stays on, the map follows
you. Drag the map to stop following; the dot keeps updating until you switch the button off.
The position is never written to the database or sent to any server: cameras come from the
local store, so neither panning nor following your location tells Overpass where you are.

Permissions: Android asks the first time (precise or approximate). On Windows, WebView2's own
"allow location?" bubble renders as an empty grey box in a Tauri window, so the app answers
that request itself; the page only asks after you press the locate button, and Windows'
location privacy setting (Settings → Privacy & security → Location) still decides. macOS and
Linux depend on the system WebView (WebKitGTK uses GeoClue).

### Camera proximity alerts (live)

While the locate button is on, every position update is checked against the cameras around
you, on the device. When you come within the alert distance of a camera (default 200 m;
100 m–1 km in Settings → Camera proximity alerts) the app shows a banner with the camera type,
distance, direction and operator, highlights it on the map, plays a short sound and vibrates
(phones). A system notification is added only when the window is not in front.

- Only the categories currently shown in Filters alert, so hiding "ALPR (vendor unknown)"
  leaves Flock-only alerts.
- Each camera alerts once; it can alert again only after you have been 1.5× the alert distance
  away and 15 minutes have passed. Fixes less precise than 500 m never trigger.
- Cameras around you (at least 1 km, or 5× the alert distance) are read from the local store
  after every 250 m of movement, independently of the map view. No network request is made.
- The screen is kept on (Screen Wake Lock) while alerts can fire.
- **Foreground only.** Alerts need the app open and in front; when Android backgrounds the app
  or the screen turns off, positions stop. Background alerts would need a native foreground
  service with background location permission and are not implemented.

### Watch-area and route alerts

- **Watch areas**: a named point plus a radius (100 m – 10 km). Right-click the map, or use
  *Watch this area* in a camera's detail panel.
- **Routes**: a drawn polyline or an imported GPX track plus a corridor width (50 m – 1 km).
  Only the grid cells the route actually passes through are queried. Route reports list
  cameras ordered by distance along the route and export to CSV or GeoJSON.
- A refresh runs shortly after launch (skipped if the last one was under an hour ago) and
  then every N hours (default 24, 6–168, or *manual only*, which makes zero background
  requests). It re-queries only the cells intersecting saved targets.
- The first check for a target records a silent baseline. Afterwards a desktop
  notification fires only for cameras that are newly present, once, per target; cameras
  that disappear are logged as removed without notifying. Overlapping areas each notify
  and the Alerts panel says which cameras are shared.
- Offline at refresh time is an expected state: it is logged, shown in the Alerts panel,
  and retried at the next interval. If the OS blocks notifications the Alerts panel still
  updates and the toolbar shows a badge.

Known limitation: desktop notification *clicks* are not delivered to Tauri apps by the
notification plugin on Linux/Windows, so clicking a notification only focuses the app. The
in-app toast ("Show") and the Alerts panel zoom to the new cameras and highlight them.

---

## Attribution and licensing of data

Camera data © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors,
available under the [Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/).
Exports produced by this app (JOSM `.osm`, CSV, GeoJSON) contain OSM-derived data and
carry the same obligations. Geocoding by
[Nominatim](https://operations.osmfoundation.org/policies/nominatim/); queries by the
[Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API) — please keep the
default endpoints' usage policies in mind if you change the TTL or refresh interval.

## Out of scope

Recording or uploading your location (history, background tracking), iOS builds, any
plate/vehicle/personal data, camera
imagery, multi-user accounts or a hosted backend, scraping vendor or municipal websites,
predicting unmapped camera locations.
