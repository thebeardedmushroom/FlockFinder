# Third-party notices

## Flock Finder (simeononsecurity/flock-finder)

The Wi-Fi fingerprint feature of this app is derived from the separately maintained
project **Flock Finder** by simeononsecurity and contributors
(<https://github.com/simeononsecurity/flock-finder>), which shares this app's name but is
otherwise unrelated. From it we:

- bundle its canonical list of suspected Flock Safety Wi-Fi OUI prefixes
  (`src-tauri/data/flock_ouis.json`), researched by **@NitekryDPaul** with one prefix
  contributed by **DeFlockJoplin**;
- re-implement its WiGLE-derived dataset format, deduplication and 730-day retention
  rules in Rust so the app can download and display its published `flock_cameras.csv`;
- follow its data policy: every record is *suspected*, coordinates are passed through
  unmodified, and stale records are pruned.

Its license (`src-tauri/data/flock_ouis.LICENSE`):

```
MIT License

Copyright (c) 2026 dagnazty

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## WiGLE

The sightings in that dataset originate from the crowdsourced WiGLE wardriving database
(<https://wigle.net>). The app never queries WiGLE itself; it downloads the derived,
published CSV from the project above. Wigle-format CSV files imported by the user are
produced by the user's own tools and stay on the user's machine.

## OpenFreeMap map styles

`src/map/styles/openfreemap-{dark,fiord,positron}.json` are snapshots of the styles OpenFreeMap
serves at `https://tiles.openfreemap.org/styles/<name>`, from
<https://github.com/hyperknot/openfreemap-styles> (MIT License, Copyright (c) 2023 Zsolt Ero).
The map themes recolour them at runtime; the tiles, fonts and sprites they reference are
fetched from OpenFreeMap.

- **Dark**: forked from [openmaptiles/dark-matter-gl-style](https://github.com/openmaptiles/dark-matter-gl-style).
- **Fiord**: forked from [openmaptiles/fiord-color-gl-style](https://github.com/openmaptiles/fiord-color-gl-style).
- **Positron**: forked from [openmaptiles/positron-gl-style](https://github.com/openmaptiles/positron-gl-style),
  itself derived from CartoDB Basemaps designed by Stamen and Paul Norman for CartoDB Inc.
  (CC BY 3.0).

The OpenMapTiles styles' code is released under the BSD 3-Clause License and their design
under CC BY 4.0. Map data © OpenStreetMap contributors; © OpenMapTiles. Fonts: Noto Sans (SIL
Open Font License 1.1). Icons: Maki (CC0 1.0). Natural Earth data: public domain. The map's
attribution control shows the OpenFreeMap / OpenMapTiles / OpenStreetMap credit on every theme.

## OpenStreetMap

Camera data © OpenStreetMap contributors, Open Database License (ODbL). See README.md.

## Android libraries for turn-by-turn navigation

- **Google Play services Location** (`com.google.android.gms:play-services-location`): the
  fused location provider and the location-settings dialog. Free and needs no API key; used
  under the [Android Software Development Kit License](https://developer.android.com/studio/terms)
  and Google APIs terms. Phones without Play services use the platform GPS provider instead.
- **AndroidX Media** (`androidx.media:media`): audio focus for spoken guidance. Apache
  License 2.0.

Spoken instructions are the Valhalla routing server's own text (see *Directions* in the
README); the maneuver icons and the camera chime are original to this app.
