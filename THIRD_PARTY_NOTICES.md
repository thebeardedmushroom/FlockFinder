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

## OpenStreetMap

Camera data © OpenStreetMap contributors, Open Database License (ODbL). See README.md.
