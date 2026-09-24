//! Wi-Fi fingerprint sightings: *suspected* Flock Safety devices inferred from a Wi-Fi
//! OUI (MAC prefix) match.
//!
//! Two sources feed the `wifi_sightings` table:
//! * the published dataset of the separate Flock Finder project
//!   (simeononsecurity/flock-finder, MIT), built from the crowdsourced WiGLE database;
//! * Wigle-format CSV files the user produced with their own wardriving tools.
//!
//! This is heuristic data. It is kept strictly apart from OSM cameras, always labelled
//! "suspected", and never used for alerts or OSM uploads.

use crate::error::{AppError, AppResult};
use crate::geo_util::valid_coord;
use crate::grid::BBox;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

pub const UPSTREAM_REPO: &str = "https://github.com/simeononsecurity/flock-finder";
pub const DATASET_CSV_URL: &str =
    "https://raw.githubusercontent.com/simeononsecurity/flock-finder/main/data/flock_cameras.csv";
pub const DATASET_STATS_URL: &str =
    "https://raw.githubusercontent.com/simeononsecurity/flock-finder/main/data/scan_stats.json";
pub const DATA_POLICY_URL: &str =
    "https://github.com/simeononsecurity/flock-finder/blob/main/docs/DATA_POLICY.md";
/// Sightings whose latest observation is older than this are dropped (mirrors upstream).
pub const RETENTION_DAYS: i64 = 730;
pub const SOURCE_UPSTREAM: &str = "upstream";
pub const SOURCE_IMPORT: &str = "wigle_import";

const OUI_JSON: &str = include_str!("../data/flock_ouis.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OuiEntry {
    pub oui: String,
    #[serde(default)]
    pub vendor_context: String,
    #[serde(default)]
    pub detection_protocol: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Deserialize)]
struct OuiFile {
    ouis: Vec<OuiEntry>,
}

/// The bundled canonical list of suspected Flock Safety OUI prefixes.
pub fn oui_list() -> &'static [OuiEntry] {
    static LIST: OnceLock<Vec<OuiEntry>> = OnceLock::new();
    LIST.get_or_init(|| {
        let file: OuiFile = serde_json::from_str(OUI_JSON).expect("bundled flock_ouis.json is valid");
        file.ouis
            .into_iter()
            .map(|mut e| {
                e.oui = e.oui.to_ascii_uppercase();
                e
            })
            .collect()
    })
}

/// Normalise a MAC to `AA:BB:CC:DD:EE:FF`. Accepts `:`/`-`/`.` separators or none.
pub fn normalize_mac(mac: &str) -> Option<String> {
    let hex: String = mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if hex.len() != 12 {
        return None;
    }
    let octets: Vec<&str> = (0..6).map(|i| &hex[i * 2..i * 2 + 2]).collect();
    Some(octets.join(":"))
}

pub fn oui_of(mac: &str) -> Option<String> {
    normalize_mac(mac).map(|m| m[..8].to_string())
}

/// The bundled OUI entry matching a MAC, if any.
pub fn match_oui(mac: &str) -> Option<&'static OuiEntry> {
    let oui = oui_of(mac)?;
    oui_list().iter().find(|e| e.oui == oui)
}

/// SSID patterns observed on Flock hardware (see the upstream README): bare `Flock`,
/// provisioning names like `Flock-1A2B3C`, `Flock001`, and `Flock Camera net.`.
pub fn ssid_looks_flock(ssid: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)^(flock|flock-[0-9a-f]{4,6}|flock\d{3}|flock camera net\.?)$").unwrap()
    });
    re.is_match(ssid.trim())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WifiSighting {
    pub netid: String,
    pub lat: f64,
    pub lon: f64,
    pub oui: String,
    pub ssid: Option<String>,
    pub channel: Option<i64>,
    pub encryption: Option<String>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub road: Option<String>,
    pub postalcode: Option<String>,
    pub source: String,
    pub imported_at: i64,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ParseStats {
    pub parsed: usize,
    pub skipped_invalid: usize,
    pub skipped_old: usize,
    pub skipped_unmatched: usize,
    pub deduplicated: usize,
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Days since the date at the start of an ISO-ish timestamp (`YYYY-MM-DD...`).
fn age_days(stamp: &str, now: i64) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(stamp.get(..10)?, "%Y-%m-%d").ok()?;
    let secs = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
    Some((now - secs) / 86_400)
}

fn too_old(last_seen: &Option<String>, now: i64) -> bool {
    match last_seen {
        Some(s) => age_days(s, now).map_or(false, |d| d > RETENTION_DAYS),
        None => false,
    }
}

/// Keep one record per BSSID, preferring the most recent `last_seen`.
fn dedupe(records: Vec<WifiSighting>, stats: &mut ParseStats) -> Vec<WifiSighting> {
    let mut best: HashMap<String, WifiSighting> = HashMap::new();
    for r in records {
        match best.get(&r.netid) {
            Some(existing) if existing.last_seen >= r.last_seen => stats.deduplicated += 1,
            Some(_) => {
                stats.deduplicated += 1;
                best.insert(r.netid.clone(), r);
            }
            None => {
                best.insert(r.netid.clone(), r);
            }
        }
    }
    let mut out: Vec<WifiSighting> = best.into_values().collect();
    out.sort_by(|a, b| a.netid.cmp(&b.netid));
    out
}

fn col<'a>(headers: &csv::StringRecord, row: &'a csv::StringRecord, name: &str) -> &'a str {
    headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
        .and_then(|i| row.get(i))
        .unwrap_or("")
}

/// Parse the upstream `flock_cameras.csv` (columns: netid, ssid, trilat, trilong,
/// oui_match, channel, encryption, firsttime, lasttime, city, region, country, road,
/// postalcode). Invalid coordinates and stale rows are skipped.
pub fn parse_upstream_csv<R: Read>(reader: R, now: i64) -> AppResult<(Vec<WifiSighting>, ParseStats)> {
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(reader);
    let headers = rdr.headers().map_err(|e| AppError::Parse(format!("dataset CSV: {e}")))?.clone();
    if !headers.iter().any(|h| h.eq_ignore_ascii_case("netid")) {
        return Err(AppError::Parse("dataset CSV has no `netid` column".into()));
    }
    let mut stats = ParseStats::default();
    let mut out = Vec::new();
    for row in rdr.records() {
        let row = match row {
            Ok(r) => r,
            Err(_) => {
                stats.skipped_invalid += 1;
                continue;
            }
        };
        let Some(netid) = normalize_mac(col(&headers, &row, "netid")) else {
            stats.skipped_invalid += 1;
            continue;
        };
        let lat: f64 = col(&headers, &row, "trilat").trim().parse().unwrap_or(f64::NAN);
        let lon: f64 = col(&headers, &row, "trilong").trim().parse().unwrap_or(f64::NAN);
        if !valid_coord(lat, lon) || (lat == 0.0 && lon == 0.0) {
            stats.skipped_invalid += 1;
            continue;
        }
        let last_seen = opt(col(&headers, &row, "lasttime"));
        if too_old(&last_seen, now) {
            stats.skipped_old += 1;
            continue;
        }
        let oui = opt(col(&headers, &row, "oui_match"))
            .and_then(|o| normalize_mac(&format!("{o}:00:00:00")).map(|m| m[..8].to_string()))
            .unwrap_or_else(|| netid[..8].to_string());
        out.push(WifiSighting {
            netid,
            lat,
            lon,
            oui,
            ssid: opt(col(&headers, &row, "ssid")),
            channel: col(&headers, &row, "channel").trim().parse().ok(),
            encryption: opt(col(&headers, &row, "encryption")),
            first_seen: opt(col(&headers, &row, "firsttime")),
            last_seen,
            city: opt(col(&headers, &row, "city")),
            region: opt(col(&headers, &row, "region")),
            country: opt(col(&headers, &row, "country")),
            road: opt(col(&headers, &row, "road")),
            postalcode: opt(col(&headers, &row, "postalcode")),
            source: SOURCE_UPSTREAM.into(),
            imported_at: now,
        });
        stats.parsed += 1;
    }
    let out = dedupe(out, &mut stats);
    Ok((out, stats))
}

/// Parse a Wigle-format wardriving CSV (WigleWifi-1.4/1.6 as written by the WiGLE app,
/// ESP32 Marauder "Flock Wardrive" and similar). Only rows whose MAC matches a bundled
/// OUI or whose SSID looks like a Flock name are kept; everything else is discarded.
pub fn parse_wigle_csv<R: Read>(reader: R, now: i64) -> AppResult<(Vec<WifiSighting>, ParseStats)> {
    let mut text = String::new();
    let mut reader = reader;
    reader
        .read_to_string(&mut text)
        .map_err(|e| AppError::Parse(format!("could not read file as UTF-8 text: {e}")))?;
    // Wigle exports start with a one-line preamble ("WigleWifi-1.4,appRelease=...").
    let body = if text.starts_with("WigleWifi") {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => "",
        }
    } else {
        text.as_str()
    };
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(body.as_bytes());
    let headers = rdr.headers().map_err(|e| AppError::Parse(format!("Wigle CSV: {e}")))?.clone();
    if !headers.iter().any(|h| h.trim().eq_ignore_ascii_case("MAC")) {
        return Err(AppError::Parse(
            "this does not look like a Wigle CSV (no `MAC` column; expected the WigleWifi export format)".into(),
        ));
    }
    let mut stats = ParseStats::default();
    let mut out = Vec::new();
    for row in rdr.records() {
        let row = match row {
            Ok(r) => r,
            Err(_) => {
                stats.skipped_invalid += 1;
                continue;
            }
        };
        let kind = col(&headers, &row, "Type").trim().to_ascii_uppercase();
        if !(kind.is_empty() || kind == "WIFI") {
            stats.skipped_unmatched += 1;
            continue;
        }
        let Some(netid) = normalize_mac(col(&headers, &row, "MAC")) else {
            stats.skipped_invalid += 1;
            continue;
        };
        let ssid = opt(col(&headers, &row, "SSID"));
        let matched = match_oui(&netid).map(|e| e.oui.clone());
        let by_ssid = ssid.as_deref().map_or(false, ssid_looks_flock);
        if matched.is_none() && !by_ssid {
            stats.skipped_unmatched += 1;
            continue;
        }
        let lat: f64 = col(&headers, &row, "CurrentLatitude").trim().parse().unwrap_or(f64::NAN);
        let lon: f64 = col(&headers, &row, "CurrentLongitude").trim().parse().unwrap_or(f64::NAN);
        if !valid_coord(lat, lon) || (lat == 0.0 && lon == 0.0) {
            stats.skipped_invalid += 1;
            continue;
        }
        let seen = opt(col(&headers, &row, "FirstSeen"));
        out.push(WifiSighting {
            oui: matched.unwrap_or_else(|| netid[..8].to_string()),
            netid,
            lat,
            lon,
            ssid,
            channel: col(&headers, &row, "Channel").trim().parse().ok(),
            encryption: opt(col(&headers, &row, "AuthMode")),
            first_seen: seen.clone(),
            last_seen: seen,
            city: None,
            region: None,
            country: None,
            road: None,
            postalcode: None,
            source: SOURCE_IMPORT.into(),
            imported_at: now,
        });
        stats.parsed += 1;
    }
    let out = dedupe(out, &mut stats);
    Ok((out, stats))
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const COLS: &str = "netid, lat, lon, oui, ssid, channel, encryption, first_seen, last_seen, city, region, country, road, postalcode, source, imported_at";

fn row_to_sighting(r: &rusqlite::Row) -> rusqlite::Result<WifiSighting> {
    Ok(WifiSighting {
        netid: r.get(0)?,
        lat: r.get(1)?,
        lon: r.get(2)?,
        oui: r.get(3)?,
        ssid: r.get(4)?,
        channel: r.get(5)?,
        encryption: r.get(6)?,
        first_seen: r.get(7)?,
        last_seen: r.get(8)?,
        city: r.get(9)?,
        region: r.get(10)?,
        country: r.get(11)?,
        road: r.get(12)?,
        postalcode: r.get(13)?,
        source: r.get(14)?,
        imported_at: r.get(15)?,
    })
}

/// Insert or refresh sightings in one transaction. An existing row is only replaced when
/// the incoming `last_seen` is at least as recent.
pub fn upsert(conn: &mut Connection, sightings: &[WifiSighting]) -> AppResult<usize> {
    let tx = conn.transaction()?;
    let mut n = 0usize;
    {
        let mut stmt = tx.prepare_cached(&format!(
            "INSERT INTO wifi_sightings({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(netid) DO UPDATE SET
               lat = excluded.lat, lon = excluded.lon, oui = excluded.oui, ssid = excluded.ssid,
               channel = excluded.channel, encryption = excluded.encryption, first_seen = excluded.first_seen,
               last_seen = excluded.last_seen, city = excluded.city, region = excluded.region,
               country = excluded.country, road = excluded.road, postalcode = excluded.postalcode,
               source = excluded.source, imported_at = excluded.imported_at
             WHERE COALESCE(excluded.last_seen, '') >= COALESCE(wifi_sightings.last_seen, '')"
        ))?;
        for s in sightings {
            n += stmt.execute(params![
                s.netid, s.lat, s.lon, s.oui, s.ssid, s.channel, s.encryption, s.first_seen, s.last_seen,
                s.city, s.region, s.country, s.road, s.postalcode, s.source, s.imported_at
            ])?;
        }
    }
    tx.commit()?;
    Ok(n)
}

pub fn in_bbox(conn: &Connection, bbox: &BBox) -> AppResult<Vec<WifiSighting>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM wifi_sightings WHERE lat >= ?1 AND lat <= ?2 AND lon >= ?3 AND lon <= ?4"
    ))?;
    let mut out = Vec::new();
    for part in bbox.sanitized().split_antimeridian() {
        let rows = stmt.query_map(params![part.south, part.north, part.west, part.east], row_to_sighting)?;
        for r in rows {
            out.push(r?);
        }
    }
    Ok(out)
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Counts {
    pub total: i64,
    pub upstream: i64,
    pub imported: i64,
}

pub fn counts(conn: &Connection) -> AppResult<Counts> {
    let mut c = Counts::default();
    let mut stmt = conn.prepare_cached("SELECT source, COUNT(*) FROM wifi_sightings GROUP BY source")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (source, n) = row?;
        c.total += n;
        if source == SOURCE_UPSTREAM {
            c.upstream += n;
        } else {
            c.imported += n;
        }
    }
    Ok(c)
}

pub fn clear(conn: &Connection, source: Option<&str>) -> AppResult<usize> {
    Ok(match source {
        Some(s) => conn.execute("DELETE FROM wifi_sightings WHERE source = ?1", params![s])?,
        None => conn.execute("DELETE FROM wifi_sightings", [])?,
    })
}

/// Drop sightings whose latest observation is beyond the retention window.
pub fn prune_old(conn: &Connection, now: i64) -> AppResult<usize> {
    let cutoff = chrono::DateTime::from_timestamp(now - RETENTION_DAYS * 86_400, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    Ok(conn.execute(
        "DELETE FROM wifi_sightings WHERE last_seen IS NOT NULL AND substr(last_seen, 1, 10) < ?1",
        params![cutoff],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    const NOW: i64 = 1_789_000_000; // 2026-09-10-ish

    #[test]
    fn bundled_oui_list_loads() {
        let list = oui_list();
        assert_eq!(list.len(), 31);
        assert!(list.iter().any(|e| e.oui == "70:C9:4E"));
        assert!(list.iter().all(|e| e.oui.len() == 8));
    }

    #[test]
    fn mac_normalisation_and_oui_matching() {
        assert_eq!(normalize_mac("70:c9:4e:12:34:56").as_deref(), Some("70:C9:4E:12:34:56"));
        assert_eq!(normalize_mac("70-C9-4E-12-34-56").as_deref(), Some("70:C9:4E:12:34:56"));
        assert_eq!(normalize_mac("70c9.4e12.3456").as_deref(), Some("70:C9:4E:12:34:56"));
        assert!(normalize_mac("not a mac").is_none());
        assert!(normalize_mac("70:c9:4e:12:34").is_none());
        assert_eq!(match_oui("70:c9:4e:aa:bb:cc").map(|e| e.oui.as_str()), Some("70:C9:4E"));
        assert!(match_oui("aa:bb:cc:dd:ee:ff").is_none());
    }

    #[test]
    fn flock_ssid_patterns() {
        for s in ["Flock", "FLOCK", "Flock-1A2B3C", "FLOCK-ABCDEF", "Flock-6361", "Flock001", "Flock Camera net.", " Flock Camera net"] {
            assert!(ssid_looks_flock(s), "{s}");
        }
        for s in ["Flock of seagulls", "ClickShare", "Flock-12", "Flockmate", "", "SMARTGATE_123456"] {
            assert!(!ssid_looks_flock(s), "{s}");
        }
    }

    const UPSTREAM: &str = "netid,ssid,trilat,trilong,oui_match,channel,encryption,firsttime,lasttime,city,region,country,road,postalcode
00:F4:8D:01:C5:30,,42.1026001,-93.56041718,00:F4:8D,8,unknown,2026-07-29T08:00:00.000Z,2026-07-29T20:00:00.000Z,,IA,US,I 35,50019
00:F4:8D:01:D3:28,Lucky Chucks_5G,43.078022,-88.48629761,00:F4:8D,2,none,2026-08-27T14:00:00.000Z,2026-08-27T13:00:00.000Z,,WI,US,Valley Road,53066
00:F4:8D:01:C5:30,,42.1026001,-93.56041718,00:F4:8D,8,unknown,2026-07-29T08:00:00.000Z,2026-08-01T20:00:00.000Z,,IA,US,I 35,50019
70:C9:4E:00:00:01,Flock,0,0,70:C9:4E,1,none,2026-01-01T00:00:00.000Z,2026-01-01T00:00:00.000Z,,,,,
70:C9:4E:00:00:02,,39.7,-105.0,70:C9:4E,6,wpa2,2023-01-01T00:00:00.000Z,2023-06-01T00:00:00.000Z,,CO,US,,
garbage,,39.7,-105.0,70:C9:4E,6,wpa2,2026-01-01,2026-01-01,,,,,
";

    #[test]
    fn parses_upstream_csv_with_retention_and_dedupe() {
        let (rows, stats) = parse_upstream_csv(UPSTREAM.as_bytes(), NOW).unwrap();
        assert_eq!(stats.skipped_invalid, 2, "null island + garbage mac");
        assert_eq!(stats.skipped_old, 1);
        assert_eq!(stats.deduplicated, 1);
        assert_eq!(rows.len(), 2);
        let first = rows.iter().find(|r| r.netid == "00:F4:8D:01:C5:30").unwrap();
        assert_eq!(first.last_seen.as_deref(), Some("2026-08-01T20:00:00.000Z"), "latest kept");
        assert_eq!(first.oui, "00:F4:8D");
        assert_eq!(first.road.as_deref(), Some("I 35"));
        assert_eq!(first.channel, Some(8));
        assert!(first.ssid.is_none());
        assert_eq!(first.source, SOURCE_UPSTREAM);
        assert!(parse_upstream_csv("a,b\n1,2\n".as_bytes(), NOW).is_err());
    }

    const WIGLE: &str = "WigleWifi-1.4,appRelease=2.53,model=Pixel,release=13,device=pixel,display=x,board=x,brand=google
MAC,SSID,AuthMode,FirstSeen,Channel,RSSI,CurrentLatitude,CurrentLongitude,AltitudeMeters,AccuracyMeters,Type
70:C9:4E:AA:BB:CC,,[WPA2-PSK-CCMP][ESS],2026-09-01 12:00:00,6,-70,39.7392,-104.9903,1600,5,WIFI
AA:BB:CC:DD:EE:01,Flock-1A2B3C,[ESS],2026-09-01 12:01:00,1,-60,39.7400,-104.9900,1600,5,WIFI
AA:BB:CC:DD:EE:02,HomeWifi,[WPA2-PSK-CCMP][ESS],2026-09-01 12:02:00,11,-80,39.7410,-104.9890,1600,5,WIFI
70:C9:4E:AA:BB:CD,,Misc,2026-09-01 12:03:00,0,-75,39.7420,-104.9880,1600,5,BLE
70:C9:4E:AA:BB:CE,,[ESS],2026-09-01 12:04:00,6,-70,0,0,0,0,WIFI
";

    #[test]
    fn parses_wigle_csv_keeping_only_flock_matches() {
        let (rows, stats) = parse_wigle_csv(WIGLE.as_bytes(), NOW).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(stats.skipped_unmatched, 2, "HomeWifi + BLE row");
        assert_eq!(stats.skipped_invalid, 1, "null island");
        let by_oui = rows.iter().find(|r| r.netid == "70:C9:4E:AA:BB:CC").unwrap();
        assert_eq!(by_oui.oui, "70:C9:4E");
        assert_eq!(by_oui.source, SOURCE_IMPORT);
        assert_eq!(by_oui.encryption.as_deref(), Some("[WPA2-PSK-CCMP][ESS]"));
        let by_ssid = rows.iter().find(|r| r.netid == "AA:BB:CC:DD:EE:01").unwrap();
        assert_eq!(by_ssid.oui, "AA:BB:CC", "SSID match keeps its own OUI");
        assert!(parse_wigle_csv("netid,ssid\n1,2\n".as_bytes(), NOW).is_err());
    }

    /// Parses the full published dataset when `FLOCK_FINDER_UPSTREAM_CSV` points at a local
    /// copy of `flock_cameras.csv`; silently passes otherwise so CI needs no download.
    #[test]
    fn parses_full_upstream_csv_if_available() {
        let Ok(path) = std::env::var("FLOCK_FINDER_UPSTREAM_CSV") else { return };
        let file = std::fs::File::open(&path).expect("upstream csv readable");
        let start = std::time::Instant::now();
        let (rows, stats) = parse_upstream_csv(std::io::BufReader::new(file), crate::db::now()).unwrap();
        eprintln!(
            "full dataset: {} rows kept, {} stale, {} invalid, {} dupes in {:?}",
            rows.len(),
            stats.skipped_old,
            stats.skipped_invalid,
            stats.deduplicated,
            start.elapsed()
        );
        assert!(rows.len() > 100_000, "expected a six-figure dataset, got {}", rows.len());
        assert!(rows.iter().all(|r| valid_coord(r.lat, r.lon)));
        assert!(rows.iter().all(|r| r.netid.len() == 17 && r.oui.len() == 8));
        let mut c = test_conn();
        let t = std::time::Instant::now();
        assert_eq!(upsert(&mut c, &rows).unwrap(), rows.len());
        eprintln!("upsert of {} rows took {:?}", rows.len(), t.elapsed());
        assert!(t.elapsed().as_secs() < 30);
    }

    #[test]
    fn storage_round_trip_bbox_counts_prune() {
        let mut c = test_conn();
        let (rows, _) = parse_upstream_csv(UPSTREAM.as_bytes(), NOW).unwrap();
        assert_eq!(upsert(&mut c, &rows).unwrap(), 2);
        let (wigle, _) = parse_wigle_csv(WIGLE.as_bytes(), NOW).unwrap();
        upsert(&mut c, &wigle).unwrap();
        let totals = counts(&c).unwrap();
        assert_eq!((totals.total, totals.upstream, totals.imported), (4, 2, 2));

        let denver = in_bbox(&c, &BBox::new(39.7, -105.0, 39.8, -104.9)).unwrap();
        assert_eq!(denver.len(), 2);

        // An older re-import must not overwrite a newer record.
        let mut older = rows[0].clone();
        older.last_seen = Some("2020-01-01T00:00:00.000Z".into());
        older.road = Some("should not win".into());
        upsert(&mut c, &[older]).unwrap();
        let kept = in_bbox(&c, &BBox::new(42.0, -94.0, 43.5, -88.0)).unwrap();
        assert!(kept.iter().all(|r| r.road.as_deref() != Some("should not win")));

        // Prune: nothing older than the window is present now; make one stale and prune it.
        c.execute("UPDATE wifi_sightings SET last_seen = '2019-01-01T00:00:00Z' WHERE netid = '00:F4:8D:01:D3:28'", []).unwrap();
        assert_eq!(prune_old(&c, NOW).unwrap(), 1);
        assert_eq!(clear(&c, Some(SOURCE_IMPORT)).unwrap(), 2);
        assert_eq!(counts(&c).unwrap().total, 1);
        assert_eq!(clear(&c, None).unwrap(), 1);
    }
}
