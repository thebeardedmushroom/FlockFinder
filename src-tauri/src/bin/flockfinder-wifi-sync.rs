//! Headless updater for the Wi-Fi fingerprint dataset.
//!
//! Runs the same download → parse → dedupe → store pipeline as Settings → "Download
//! dataset", but from the command line so it can be scripted (cron, a weekly task, CI).
//! Safe to run while the app is open: SQLite is in WAL mode and the app re-queries the
//! table on its next viewport change or when its window regains focus.
//!
//! Usage:
//!   flockfinder-wifi-sync [--db <path>] [--csv <local flock_cameras.csv>] [--clear]
//!
//! Defaults to the app's own database in the platform app-data directory.

use flockfinder_lib::db;
use flockfinder_lib::http::HttpClient;
use flockfinder_lib::wifi;
use std::path::PathBuf;

fn default_db_path() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("org.flockfinder.app")
        .join("flockfinder.sqlite")
}

fn usage() -> ! {
    eprintln!("usage: flockfinder-wifi-sync [--db <path>] [--csv <local flock_cameras.csv>] [--clear]");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut db_path = default_db_path();
    let mut csv_path: Option<PathBuf> = None;
    let mut clear_only = false;
    let mut view: Option<(f64, f64, f64)> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--db" => db_path = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--csv" => csv_path = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "--clear" => clear_only = true,
            // Also set where the map opens next launch: --view lat,lon,zoom
            "--view" => {
                let spec = args.next().unwrap_or_else(|| usage());
                let parts: Vec<f64> = spec.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if parts.len() != 3 {
                    usage();
                }
                view = Some((parts[0], parts[1], parts[2]));
            }
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }

    let mut conn = match db::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };
    if let Err(e) = db::migrate(&mut conn) {
        eprintln!("migration failed: {e}");
        std::process::exit(1);
    }
    println!("database: {}", db_path.display());

    if let Some((lat, lon, zoom)) = view {
        db::set_json(&conn, "last_view", &serde_json::json!({ "lat": lat, "lon": lon, "zoom": zoom }))
            .expect("set view");
        println!("start view set to {lat}, {lon} @ z{zoom}");
    }

    if clear_only {
        let n = wifi::clear(&conn, Some(wifi::SOURCE_UPSTREAM)).unwrap_or(0);
        let _ = db::set_json(&conn, "wifi_dataset", &serde_json::json!({}));
        println!("removed {n} upstream sightings");
        return;
    }

    let now = db::now();
    let (bytes, generated, upstream_total): (Vec<u8>, Option<String>, Option<i64>) = match &csv_path {
        Some(p) => {
            println!("reading {}", p.display());
            (std::fs::read(p).expect("csv readable"), None, None)
        }
        None => {
            let http = HttpClient::new().expect("http client");
            println!("fetching {}", wifi::DATASET_STATS_URL);
            let stats: serde_json::Value = match http.client.get(wifi::DATASET_STATS_URL).send().await {
                Ok(r) => r.json().await.unwrap_or(serde_json::Value::Null),
                Err(e) => {
                    eprintln!("stats fetch failed: {e}");
                    serde_json::Value::Null
                }
            };
            println!("fetching {} (about 24 MB)", wifi::DATASET_CSV_URL);
            let resp = match http
                .send_with_backoff("Flock Finder dataset", || {
                    http.client.get(wifi::DATASET_CSV_URL).timeout(std::time::Duration::from_secs(600))
                })
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("download failed: {e}");
                    std::process::exit(1);
                }
            };
            let bytes = resp.bytes().await.expect("body").to_vec();
            (
                bytes,
                stats.get("scan_timestamp").and_then(|v| v.as_str()).map(String::from),
                stats.get("total_cameras").and_then(|v| v.as_i64()),
            )
        }
    };
    println!("downloaded {} bytes", bytes.len());

    let (rows, stats) = match wifi::parse_upstream_csv(bytes.as_slice(), now) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("parse failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "parsed {} rows ({} stale, {} invalid, {} duplicates dropped)",
        rows.len(),
        stats.skipped_old,
        stats.skipped_invalid,
        stats.deduplicated
    );

    wifi::clear(&conn, Some(wifi::SOURCE_UPSTREAM)).expect("clear");
    let inserted = wifi::upsert(&mut conn, &rows).expect("upsert");
    let pruned = wifi::prune_old(&conn, now).unwrap_or(0);
    db::set_json(
        &conn,
        "wifi_dataset",
        &serde_json::json!({
            "downloaded_at": now,
            "upstream_generated": generated,
            "upstream_total": upstream_total,
        }),
    )
    .expect("meta");
    let counts = wifi::counts(&conn).expect("counts");
    println!(
        "stored {inserted} sightings ({pruned} pruned); table now holds {} ({} upstream, {} imported)",
        counts.total, counts.upstream, counts.imported
    );
}
