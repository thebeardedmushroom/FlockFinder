# Flock Finder

A cross-platform Tauri v2 app (Windows, macOS, Linux and Android) that maps automated license plate reader (ALPR)
cameras — Flock Safety and other vendors — from crowdsourced **OpenStreetMap** data. It
shows an interactive map, lets you record cameras you have personally observed, tells
you when new cameras appear inside places or along routes you save, and plans driving
directions that avoid mapped cameras where it can.

> **Crowdsourced data — coverage is incomplete. Absence of a marker does not mean absence
> of a camera.** OSM only contains what volunteers have surveyed. An empty map means
> nobody has mapped that area yet.

The app maps fixed hardware only. It never handles license plate data, vehicle data, or
information about individuals. Your own location is used only while the locate button is on,
to show where you are and (optionally) to warn you when you come near a mapped camera; it is
never stored or uploaded. Watch-area and route alerts are computed against places and routes
you explicitly save, not your live position. Directions send the start and destination you
enter to a routing server (see [Directions](#directions-that-avoid-cameras)).

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

### Map themes

Five basemap themes, picked under **Settings → Basemap → Map theme** or stepped through with
the half-filled-circle button above the zoom controls. A change applies at once, keeps the
map's centre, zoom and bearing, and is remembered on the device (`localStorage` key
`flockfinder.mapTheme`, next to the proximity-alert preferences).

| Theme | Ground | Built from |
|---|---|---|
| Midnight | dark | OpenFreeMap `dark` with the app's mute pass (the original look) |
| Navy | dark | OpenFreeMap `fiord`, major roads lifted to a soft white |
| Night Vision | dark | authored palette on the `dark` layers: near-black, amber/red top road tiers |
| Paper | light | OpenFreeMap `positron` as published |
| Muted | light | authored palette on the `positron` layers: flat stone tones, subtle water |

- Until a theme is picked, the map follows the OS: Paper on a light OS, Midnight on a dark
  one. The desktop window keeps a dark title bar, and forcing that in `tauri.conf.json`
  would also force the webview's `prefers-color-scheme`. So the window starts unforced, and
  Rust reads the OS setting before setting it dark (`AppInfo.system_theme`). On Android the
  webview's media query is used.
- A saved theme id that no longer exists falls back to that default. An install whose style
  URL setting was changed before themes existed keeps it (**Custom style URL**, which also
  still accepts any MapLibre style and gets the mute pass).
- Camera markers, clusters, the density field, selections, watch areas, routes and Wi-Fi
  sightings are recoloured per theme (`OverlayPalette` in `src/map/themes.ts`). Light
  grounds get dark-rimmed markers, dark selection rings, and a ramp that runs pale → dark,
  so dense areas still stand out. The legend bar follows it. `src/__tests__/themes.test.ts`
  checks every mark against every theme's land, parks and water for a 3:1 contrast ratio
  (WCAG non-text contrast).
- App chrome (toolbar, panels, HUD, attribution) stays dark on every theme. Each of those has
  its own opaque background, so it reads the same on a light map.

Where things live: base styles are snapshots of OpenFreeMap's in `src/map/styles/`
(refresh with `node scripts/update-basemap-styles.mjs`). Palettes and theme definitions are
in `src/map/themes.ts`. The transforms that apply a palette by layer role (land, water, road
tier, casing, labels…) are in `src/map/basemap.ts`. **Adding a theme:** add its id to
`THEME_IDS` and an entry to `THEMES`: a base style, a `palette` with any roles to change
(every `BasemapPalette` key for a fully authored look), and `DARK_OVERLAY`,
`LIGHT_OVERLAY` or a variant. Then run `npm test`, which fails with the offending
mark/ground pairs if anything drops below 3:1. Settings and the map button pick it up
automatically.

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
- Basemap tiles, glyphs and sprites are the one thing loaded by the webview directly, from
  keyless OpenFreeMap (see [Map themes](#map-themes)). The theme styles themselves are
  bundled, so switching needs no request. Migration 0003 moves installs that were on the
  earlier `liberty` default style URL to `dark`; custom URLs are left alone.

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
you at whatever zoom you pick (MapLibre's stock control re-fits to zoom 15 on every fix, which
on a phone undid a zoom within a second; see `FollowingGeolocateControl` in `MapView.tsx`).
Drag the map to stop following; the dot keeps updating until you switch the button off.
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

### Directions that avoid cameras

*Directions* (toolbar, or right-click the map → *Directions from here / to here*) takes a
start and a destination and always shows two driving routes: **Fastest**, and **Avoidance**,
which avoids mapped cameras where it can. The avoidance route is drawn solid, the other
dashed, both on a casing so they read on every map theme; click a route or its card to
choose it. Each card shows distance, time and cameras passed, e.g. *Avoidance: 14.2 mi,
26 min, 0 cameras (+4 min vs fastest)*. Cameras still on the chosen route are ringed in
orange and listed with the reason they couldn't be avoided. The panel also has a
turn-by-turn list; clicking a step or a camera zooms to it.

- **Start and destination**: type an address or place (geocoded by Nominatim when you
  press Enter or *Get route*; several matches give a list to pick from), type `lat, lon`,
  pick a point on the map (*Map*), or on Android start from your position (*Me*). Desktop
  has no automatic current location.
- **What counts as passing a camera**: coming within 30 m of it (`AVOID_RADIUS_M` in
  `src-tauri/src/routing.rs`). Every camera is treated as seeing all directions in this
  version; each camera's `direction` tag is carried with it for directional avoidance later.
  Mapped OSM cameras and all your submissions count, unverified ones included (a submission
  within 15 m of a mapped camera counts as that camera). Stale cameras don't count.
- **How the avoidance route is found**: routes come from [Valhalla](https://github.com/valhalla/valhalla),
  in up to two phases (`src-tauri/src/routing.rs`): the exclusion search, then the road-map
  search for trips up to 60 km or stretch by stretch for longer ones.
  1. *Exclusion search* (enough for most trips). Ask for the fastest route (with
     alternatives), exclude the cameras that route passes, and ask again, until the route
     passes none. At most 8 requests go out, one per second, and a public Valhalla server
     accepts at most 50 excluded cameras per request. Only cameras inside the
     start/destination box, plus a buffer of 3 km or 30 % of the trip (whichever is larger),
     are excluded. When excluding a camera leaves no route at all, the camera nearest the
     start or destination is given up and the rest are tried again.
  2. *Road-map search* (only when phase 1 left cameras that aren't at the start or
     destination). In a dense metro corridor with thousands of cameras, phase 1 can stop
     before it finds a camera-free route that exists: the server only learns about the
     cameras the routes it proposes happen to pass, and each new proposal passes new ones.
     So the drivable road network for the trip area is downloaded from Overpass
     (`src-tauri/src/roadnet.rs`). The area is every 0.1° tile within 6 km or 25 % of the
     trip (whichever is larger) of the straight start–destination line; a detour around a
     camera-lined corridor has to reach the next parallel one, and the one from Buckhead to
     Sandy Springs runs 4.1 km out. One-way, no-access and reversible roads (express lanes
     whose direction flips by time of day, which Valhalla never uses) are respected. Every
     road segment within 30 m of a camera is removed and the rest searched.
     - If a camera-free path exists, Valhalla drives it in legs of about 4 km, each through
       8 waypoints on the path. Any camera it strays onto is excluded and the leg asked
       again.
     - If Valhalla won't drive a leg at all (a much longer detour, or no route: a turn
       restriction, gate or closure the road map doesn't show), that leg is closed on the
       map and the rest re-planned from its start, up to 4 times. At most 40 requests go
       out in this phase.
     - Only if the road map has no camera-free path does the search fall back to the path
       with the fewest cameras.

     Tiles download two at a time, the ones holding the start and destination first, then
     nearest the line. The download has a budget of 3 minutes. A camera-free route found on
     a partial map is real, but "none exists" is never claimed from one; the panel says the
     map was only partly downloaded instead. When Overpass is busy (HTTP 429) the download
     waits for a free slot as its status page reports; when it's overloaded (504 or a
     timed-out query) it waits 10, 20, 40, then 60 s. The road map is saved on the device
     for 30 days (Settings → *Clear cache* removes it), so a repeat trip through the area
     downloads nothing. A first trip through a dense metro can take two to three minutes;
     the panel shows each step.

  3. *Stretch by stretch* (trips over 60 km, instead of phase 2: one road map for the whole
     trip would be too big a download). On a long trip the cameras left after phase 1 are
     bunched in towns and at interchanges, and the road between is clean. So the route is
     cut into stretches around each group of cameras: 4 km before the first to 4 km after
     the last, with the cuts mid-block and clear of cameras. Each stretch is fixed on its
     own, the stretch with the most cameras first:
     - route through it with just its cameras excluded (up to 3 requests; a handful of
       cameras, well under the server's cap);
     - if cameras are still left, run the road-map search for that stretch alone (a few
       tiles), for stretches up to 40 km and in the first 4 minutes.

     The fixed stretches are spliced into the route. Work and downloads grow with the
     number of camera groups, not with the trip's length. At most 40 requests go out in
     this phase (`src-tauri/src/routing_repair.rs`). On a 600-mile Atlanta → Marion, Ohio
     trip, the first search left 17 cameras (fastest route: 23). A stretch's fix is only used
     when it adds at most a set time per camera it avoids: Settings → Directions → *Detour
     limit on long trips*, 5 minutes by default, or 2, 10, 15 or no limit. Otherwise the
     stretch keeps its road, and its cameras say so. The limit also applies to rerouting
     during navigation. On that trip, measured live (38 requests, 1.5–2.5 minutes each):

     | Detour limit | Cameras left | Slower than fastest |
     | --- | --- | --- |
     | 5 min (default) | 7 | 38 min |
     | 10 min | 3 | 1 h 13 min |
     | none | 2 | 1 h 26 min |

  Cameras within 30 m of the start or destination are never excluded. The route with the
  fewest cameras from either phase (then the quickest) is the avoidance route. Every count
  shown comes from the one rule above applied to the geometry shown.
- **When it can't avoid everything** the panel says what was actually established:
  - "No camera-free route exists", only when the road map shows there isn't one;
  - "one may exist", when the road map couldn't be fully checked (offline, Overpass busy),
    or a long trip's stretch couldn't be fixed within the limits (the panel says how many
    stretches were fixed);
  - that the first search reached the server's limits (50 excluded cameras per request, 8
    requests), whenever it did.

  Each remaining camera says why it's still there: at the start or destination; no way
  around it on the road map; the route passes it on a cross street; routing around it left
  no route; or the search hit its limits. They're listed and ringed on the map. An
  avoidance route more than 50 % or 20 minutes slower than the fastest one is flagged as a
  long detour.
- **Errors** (server unreachable, rate limited, no road near a point, points on separate road
  networks, start and destination too close) appear in the panel. The map keeps the last
  route it showed. Routes aren't recomputed when camera data changes.
- **Privacy**: the start, the destination and the excluded cameras go to the routing server.
  The default is the public server FOSSGIS e.V. runs at `valhalla1.openstreetmap.de` (free,
  no key, fair use), and requests carry `X-Client-Id: FlockFinder`. Settings → *Directions*
  takes the address of your own Valhalla instance instead. Typed addresses go to Nominatim.
  When the road-map search runs, Overpass (the endpoint in Settings) sees the trip area
  (not the start and destination themselves). Nothing about a trip is stored; the saved
  road map is just roads.
- Before distributing a release that uses the public server, FOSSGIS asks app authors to
  announce the app in the Valhalla GitHub Discussions.

Tests:
- `cargo test --lib routing` and `cargo test --lib roadnet` cover the planner against fake
  servers and road maps: both phases, unavoidable and blocking cameras, endpoints, server
  limits and failures. They also cover road parsing, tile storage, the graph search, and
  cutting and stitching legs.
- Three real trips replay recorded answers, with fixed camera data and the recorded road map
  in `src-tauri/fixtures/routing/`, so they don't need the live services:
  - Buckhead → Sandy Springs and Decatur → Marietta, which phase 1 alone got wrong;
  - Atlanta Midtown → airport, which must stay as it was.
- `cargo test --lib record_dense_metro_fixtures -- --ignored --nocapture` re-records them.
  Needed after a change to the requests the planner makes. `FLOCKFINDER_RECORD_CASE=<case>`
  records one. `FLOCKFINDER_RECORD_ROADS=<file>` takes the road map from a saved Overpass
  answer (`out body` ways plus `out skel` nodes) instead of downloading it, and
  `FLOCKFINDER_RECORD_OVERPASS=<url>` downloads it from another Overpass server. By default
  the road map already recorded for a case is reused and only the routing answers are
  recorded again (`FLOCKFINDER_RECORD_FRESH_ROADS=1` downloads it again). The committed road
  maps were downloaded from overpass-api.de on 2026-09-25.
- `src/__tests__/directions.test.ts` covers formatting and the outcome messages. The theme
  tests check the route colours against every theme.

### Saved places and quick navigation

Home, Work and up to 10 other places (Settings → *Saved places*) show as chips under the
toolbar. Tapping one plans a route from your current location with the usual settings
(camera avoidance included) and shows the preview, with *Start navigation* on Android.
They're also offered in the Directions fields. To save a searched address or a dropped pin,
use *☆ Save place* in its detail sheet or in the map's right-click / long-press menu.

- Places are stored in SQLite (`saved_places`, migration 0008; `src-tauri/src/places.rs`) on
  each device and aren't synced. Each keeps the coordinates of the search result or pin it
  was saved from, so a trip never geocodes the address again.
- An address must be chosen from the search results to be saved. A dropped pin's address is
  looked up at street level with Nominatim.
- Without a location fix within 15 s, Android explains what's missing and links to the
  setting that fixes it. The desktop opens Directions with the destination filled in and
  asks for a start. Within 50 m of the place, it says you're already there.
- `src/__tests__/places.test.ts` and the `places` Rust tests cover ordering, label rules and
  limits.

### Turn-by-turn navigation (Android)

**Start avoidance navigation** (or *fastest*) under a planned route guides you along it:
- a maneuver banner (icon, instruction, distance; the turn after it too when it's under
  150 m on);
- spoken prompts, and camera alerts ahead on the route;
- rerouting in the same mode when you leave the route.

It runs as a foreground service, so it keeps going with the screen off, the app in the
background, or the app swiped away from recents.

How it's built:

| Part | Where | What it does |
| --- | --- | --- |
| Session | `src-tauri/src/nav/session.rs` | State machine: Navigating → OffRoute → Rerouting → Navigating → Arrived, plus LostSignal and Paused. Pure logic, fed fixes and ticks, returns effects. |
| Snapping | `nav/snap.rs` | Matches each fix to the route within a window around progress so far, with a penalty for heading the wrong way. Keeps it off parallel roads, the other carriageway, and roads crossed later. |
| Prompts | `nav/guidance.rs` | Speaks the routing server's own words (Valhalla's `verbal_*` instructions). Times them at 1 mi (highway) or ½ mi, 500 ft, and at the turn. Drops a stage there's no time to say; never repeats one. |
| Camera alerts | `nav/cameras.rs` | Cameras on the route ahead, at ¼ mi and 500 ft, once each per trip. Groups nearby cameras. Says "unverified" for your own submissions. |
| Reroute | `nav/reroute.rs` | Fastest: one request. Avoidance: first rejoin the avoidance route ~1.5 km ahead (1–3 requests); if that adds cameras, re-plan with the shared planner (`routing.rs`), capped at 6 + 12 requests and cached road tiles only. |
| Simulator | `nav/sim.rs` | Drives the route with GPS noise, leaves it on demand, drops the signal, or replays a GPX track. |
| Runtime | `nav/runtime.rs` | Runs the session in the app process (not the WebView, which is throttled with the screen off). Publishes `nav:state` / `nav:route` to the screen. |
| Android | `gen/android/.../nav/` | `NavigationPlugin` (permissions, location-settings dialog, keep-screen-on), `NavigationService` (foreground service of type location, notification with *End navigation*, wake lock), `LocationSource` (Play services' fused provider, platform GPS fallback), `VoiceGuide` (TTS, ducking, calls). |
| Screen | `src/components/NavigationView.tsx`, `src/map/navCamera.ts` | Banner, bottom bar (ETA, remaining, cameras ahead, End), follow mode (heading up while moving, north-up toggle, Recenter, auto-recenter after 10 s), arrival summary. |

Rules worth knowing:
- **Off route**: more than 40 m from the route for 5 s while moving (≥ 2 m/s) with a fix
  better than 50 m.
- **Reroute rate**: at most one attempt every 10 s. Waits grow to 20/40/60 s after
  failures, and guidance stays on the old route meanwhile. Routing requests stay spaced 1 s
  apart. If a new route passes more cameras than the old one's remainder, a notice says so.
- **Signal loss**: no fix for 4 s shows *GPS signal lost*. Progress is dead-reckoned at the
  last speed for 30 s, then guidance pauses until a fix returns. No reroutes meanwhile.
- **Arrival**: within 30 m of the destination, or past it along the route. A summary
  (time, distance, cameras passed) returns to the map after 10 s.
- **Voice**: each prompt takes transient, duckable audio focus with navigation audio
  attributes, so music ducks and it follows Bluetooth. A newer prompt replaces one still
  waiting. Nothing is spoken during a call. No US English voice means an on-screen notice
  and visual guidance only.
- **Proximity alerts**: the map's own proximity alerts skip cameras the session covers, so
  no camera is announced twice. Cameras off the route still alert as before.
- **Safety**: large tap targets. The address fields lock above 5 mph.
- **Process death**: if Android kills the app mid-trip, the next launch asks *Resume
  navigation to …?* and never resumes silently. That needs one saved record (destination
  and mode), deleted when the trip ends.
- **Permissions**: precise location is asked for at the first Start. Approximate-only
  explains why precise is needed and offers the app settings. Notifications are asked for
  on Android 13+; navigation works without them. There's no background-location permission,
  because the foreground service covers it. If location is turned off, Start offers the
  system dialog; turning it off mid-trip ends guidance with a message.
- **Battery saver**: when battery saver is set to turn GPS off (or throttle it) with the
  screen off, a warning says to keep the screen on or turn battery saver off. Some
  manufacturers' own battery managers throttle foreground services without saying so. If
  fixes stop with the screen off, guidance shows *GPS signal lost*. Excluding Flock Finder
  from battery optimisation fixes it.
- **Privacy**: positions stay on the device. Only a reroute sends your position and
  destination to the routing server (Settings → Directions says which). No location history
  is kept.

**Simulator** (debug builds only): under a planned route, *Simulate drive* drives it (30 mph,
4 m GPS noise), and *Replay GPX…* replays a recorded track at its own timestamps. On the
navigation screen, the *Simulator* menu can:
- leave the route (a right turn for 400 m, then back);
- drop the signal for 40 s;
- change speed and noise;
- run at 1–8× speed.

The simulator also runs on the desktop, which has no voice, to try the screen. For real
location on the emulator, Android's developer-options mock location and `adb emu geo fix`
both work: the fused provider passes mock fixes through.

Tests (`cargo test --lib nav::`) cover snapping, prompt timing, camera alerts, the simulator
and rerouting. Whole drives on the real Midtown → airport route replay GPX tracks from
`src-tauri/fixtures/nav/`:
- a full drive: every turn prompted at the right distance, all 7 cameras alerted once and
  passed, arrival;
- leaving the route: one reroute 5–7 s after going 40 m off, still in avoidance mode;
- the rate limit and its backoff;
- a 40 s tunnel: dead reckoning, pause, no reroute;
- weak fixes, a 5-minute stop, a start far from the route, and driving past the destination.

The tracks were recorded by the simulator (`cargo test --lib record_nav_tracks -- --ignored`),
not on a real drive. `src/__tests__/nav.test.ts` covers the screen's helpers.
`node scripts/gen-nav-assets.mjs` regenerates the notification icons (from
`src/lib/maneuverIcons.json`) and the camera chime.

Known limits: Android only (out of scope: Android Auto, lanes, speed limits, traffic,
offline routing, walking, multi-stop). Tauri 2.11 on Android doesn't rebuild the WebView
when the app is reopened after being swiped away while a foreground service keeps it alive
([tauri#15671](https://github.com/tauri-apps/tauri/issues/15671)). The app works around
this by building the window again on resume (`lib.rs`).

---

## Attribution and licensing of data

Camera data © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors,
available under the [Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/).
Exports produced by this app (JOSM `.osm`, CSV, GeoJSON) contain OSM-derived data and
carry the same obligations. Geocoding by
[Nominatim](https://operations.osmfoundation.org/policies/nominatim/); directions by
[Valhalla](https://github.com/valhalla/valhalla) on the FOSSGIS public server (routing data
© OpenStreetMap contributors); queries by the
[Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API) — please keep the
default endpoints' usage policies in mind if you change the TTL or refresh interval.
Basemap tiles by [OpenFreeMap](https://openfreemap.org) © OpenMapTiles, data from
OpenStreetMap. The bundled map styles are OpenFreeMap's (MIT), derived from OpenMapTiles
styles (code BSD-3-Clause, design CC BY 4.0); see `THIRD_PARTY_NOTICES.md`.

## Out of scope

Recording or uploading your location (history, background tracking), iOS builds, any
plate/vehicle/personal data, camera
imagery, multi-user accounts or a hosted backend, scraping vendor or municipal websites,
predicting unmapped camera locations.
