//! Overpass API client: query construction, response parsing and classification.

use crate::error::{AppError, AppResult};
use crate::geo_util::valid_coord;
use crate::grid::BBox;
use crate::http::HttpClient;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_ENDPOINT: &str = "https://overpass-api.de/api/interpreter";

/// Bundled sample response (Denver metro, fetched 2026-09-10). Used by tests and by
/// the `FLOCKFINDER_OFFLINE_FIXTURE=1` development mode.
pub const SAMPLE_FIXTURE: &str = include_str!("../fixtures/overpass_sample.json");

/// Tags surfaced in the detail panel when present.
pub const DISPLAY_TAGS: [&str; 8] = [
    "direction",
    "operator",
    "brand",
    "manufacturer",
    "surveillance:zone",
    "camera:mount",
    "start_date",
    "ref",
];

/// Server-side timeout for the worldwide sync query, seconds. The query took 203 s of server
/// time on overpass-api.de on 2026-09-11 (151k elements), so 300 s would leave too little margin.
pub const GLOBAL_TIMEOUT_SECS: u32 = 600;

/// The three camera clauses, each followed by `area` (a bbox filter, or empty for the world).
fn query_body(timeout_secs: u32, area: &str) -> String {
    format!(
        "[out:json][timeout:{timeout_secs}];\n(\n  node[\"man_made\"=\"surveillance\"][\"surveillance:type\"=\"ALPR\"]{area};\n  way[\"man_made\"=\"surveillance\"][\"surveillance:type\"=\"ALPR\"]{area};\n  node[\"man_made\"=\"surveillance\"][\"surveillance:zone\"=\"traffic\"][\"camera:type\"=\"fixed\"][\"brand\"~\"[Ff]lock\"]{area};\n);\nout center tags;"
    )
}

pub fn build_query(bbox: &BBox) -> String {
    query_body(60, &format!("({})", bbox.overpass()))
}

/// Every camera in the world, in one request (the periodic sync).
pub fn build_global_query() -> String {
    query_body(GLOBAL_TIMEOUT_SECS, "")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Flock,
    Alpr,
    Unknown,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Flock => "flock",
            Category::Alpr => "alpr",
            Category::Unknown => "unknown",
        }
    }
}

/// Classify an OSM element into exactly one category.
///
/// * `flock`   — any of `brand`, `manufacturer`, `operator` matches /flock/i
/// * `alpr`    — `surveillance:type=ALPR` present, no Flock match
/// * `unknown` — anything else that the query returned
pub fn classify(tags: &BTreeMap<String, String>) -> Category {
    for key in ["brand", "manufacturer", "operator"] {
        if let Some(v) = tags.get(key) {
            if v.to_lowercase().contains("flock") {
                return Category::Flock;
            }
        }
    }
    if tags
        .get("surveillance:type")
        .map_or(false, |v| v.eq_ignore_ascii_case("ALPR"))
    {
        return Category::Alpr;
    }
    Category::Unknown
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedElement {
    pub osm_type: String,
    pub osm_id: i64,
    pub lat: f64,
    pub lon: f64,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone)]
pub struct ParsedResponse {
    pub elements: Vec<ParsedElement>,
    /// Elements dropped because they were malformed (missing id/coords, bad numbers...).
    pub skipped: usize,
    /// `osm3s.timestamp_osm_base`: the moment of the OSM database the answer reflects.
    pub osm_base: Option<String>,
}

/// Parse an Overpass JSON body. Zero elements is a success. Individual malformed
/// elements are skipped and logged; only a structurally broken body is an error.
pub fn parse_response(body: &str) -> AppResult<ParsedResponse> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("Overpass response is not valid JSON: {e}")))?;

    if let Some(remark) = value.get("remark").and_then(|r| r.as_str()) {
        // Overpass returns HTTP 200 with a remark (and possibly partial data) on timeouts
        // and memory exhaustion. Partial data must never be cached as a complete cell.
        if remark.to_lowercase().contains("runtime error") {
            return Err(AppError::Parse(format!("Overpass reported: {remark}")));
        }
    }

    let elements = value
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| AppError::Parse("Overpass response has no `elements` array".into()))?;

    let mut out = ParsedResponse {
        osm_base: value
            .pointer("/osm3s/timestamp_osm_base")
            .and_then(|t| t.as_str())
            .map(String::from),
        ..Default::default()
    };
    for el in elements {
        match parse_element(el) {
            Some(parsed) => out.elements.push(parsed),
            None => {
                out.skipped += 1;
                log::warn!(
                    "skipping malformed Overpass element: {}",
                    truncate(&el.to_string(), 200)
                );
            }
        }
    }
    Ok(out)
}

fn parse_element(el: &serde_json::Value) -> Option<ParsedElement> {
    let osm_type = el.get("type")?.as_str()?;
    if !matches!(osm_type, "node" | "way" | "relation") {
        return None;
    }
    let osm_id = el.get("id")?.as_i64()?;
    let coords = if osm_type == "node" {
        el
    } else {
        el.get("center")?
    };
    let lat = coords.get("lat")?.as_f64()?;
    let lon = coords.get("lon")?.as_f64()?;
    if !valid_coord(lat, lon) {
        return None;
    }
    let mut tags = BTreeMap::new();
    if let Some(obj) = el.get("tags").and_then(|t| t.as_object()) {
        for (k, v) in obj {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            tags.insert(k.clone(), s);
        }
    }
    Some(ParsedElement {
        osm_type: osm_type.to_string(),
        osm_id,
        lat,
        lon,
        tags,
    })
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Fetch and parse every camera inside `bbox` (which must not cross the antimeridian —
/// callers split first).
pub async fn fetch(http: &HttpClient, endpoint: &str, bbox: &BBox) -> AppResult<ParsedResponse> {
    if std::env::var("FLOCKFINDER_OFFLINE_FIXTURE").map_or(false, |v| v == "1") {
        log::info!("FLOCKFINDER_OFFLINE_FIXTURE=1: serving bundled fixture instead of Overpass");
        let mut parsed = parse_response(SAMPLE_FIXTURE)?;
        parsed.elements.retain(|e| bbox.contains(e.lat, e.lon));
        return Ok(parsed);
    }

    let query = build_query(bbox);
    let endpoint = endpoint.to_string();
    let resp = http
        .send_with_backoff("Overpass", || {
            http.client.post(&endpoint).form(&[("data", query.as_str())])
        })
        .await?;
    let body = resp.text().await?;
    parse_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MALFORMED: &str = include_str!("../fixtures/overpass_malformed.json");

    fn tags(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn query_contains_all_three_clauses_and_bbox() {
        let q = build_query(&BBox::new(39.7, -105.0, 39.75, -104.95));
        assert!(q.contains("[out:json][timeout:60];"));
        assert!(q.contains("node[\"man_made\"=\"surveillance\"][\"surveillance:type\"=\"ALPR\"](39.700000,-105.000000,39.750000,-104.950000);"));
        assert!(q.contains("way[\"man_made\"=\"surveillance\"]"));
        assert!(q.contains("[\"brand\"~\"[Ff]lock\"]"));
        assert!(q.trim_end().ends_with("out center tags;"));
    }

    #[test]
    fn global_query_has_the_same_clauses_without_a_bbox() {
        let q = build_global_query();
        assert!(q.starts_with("[out:json][timeout:600];"));
        assert!(q.contains("node[\"man_made\"=\"surveillance\"][\"surveillance:type\"=\"ALPR\"];"));
        assert!(q.contains("way[\"man_made\"=\"surveillance\"][\"surveillance:type\"=\"ALPR\"];"));
        assert!(q.contains("[\"brand\"~\"[Ff]lock\"];"));
        assert!(!q.contains("),("), "no bbox");
        // The snapshot workflow (.github/workflows/camera-snapshot.yml) sends this file.
        let file = include_str!("../data/global_query.overpassql").replace("\r\n", "\n");
        assert_eq!(q.trim(), file.trim());
        assert!(q.trim_end().ends_with("out center tags;"));
    }

    #[test]
    fn classify_flock_by_any_of_three_tags() {
        assert_eq!(classify(&tags(&[("brand", "Flock Safety")])), Category::Flock);
        assert_eq!(classify(&tags(&[("manufacturer", "flock")])), Category::Flock);
        assert_eq!(
            classify(&tags(&[("operator", "FLOCK GROUP INC"), ("surveillance:type", "ALPR")])),
            Category::Flock
        );
    }

    #[test]
    fn classify_alpr_without_flock() {
        assert_eq!(
            classify(&tags(&[("surveillance:type", "ALPR"), ("manufacturer", "Axon Enterprise")])),
            Category::Alpr
        );
        assert_eq!(classify(&tags(&[("surveillance:type", "alpr")])), Category::Alpr);
    }

    #[test]
    fn classify_unknown_otherwise() {
        assert_eq!(classify(&tags(&[("man_made", "surveillance")])), Category::Unknown);
        assert_eq!(classify(&BTreeMap::new()), Category::Unknown);
        // "Flocking" in an unrelated tag does not count.
        assert_eq!(classify(&tags(&[("name", "Flock of birds")])), Category::Unknown);
    }

    #[test]
    fn parses_real_sample_fixture() {
        let parsed = parse_response(SAMPLE_FIXTURE).unwrap();
        assert_eq!(parsed.elements.len(), 453);
        assert_eq!(parsed.skipped, 0);
        let flock = parsed
            .elements
            .iter()
            .filter(|e| classify(&e.tags) == Category::Flock)
            .count();
        let alpr = parsed
            .elements
            .iter()
            .filter(|e| classify(&e.tags) == Category::Alpr)
            .count();
        assert!(flock > 300, "flock={flock}");
        assert!(alpr > 30, "alpr={alpr}");
        assert!(parsed.elements.iter().all(|e| valid_coord(e.lat, e.lon)));
    }

    #[test]
    fn skips_malformed_elements_without_failing() {
        let parsed = parse_response(MALFORMED).unwrap();
        let ids: Vec<i64> = parsed.elements.iter().map(|e| e.osm_id).collect();
        // Kept: 1001 (ok), 2002 (way with center), 1005 (no tags), 1006 (brand=flock), 3001 (relation with center)
        assert_eq!(ids, vec![1001, 2002, 1005, 1006, 3001]);
        // Dropped: 1002 (no lat), 1003 (lat string), 1004 (lon 200.5), 2001 (way w/o center), unnamed node w/o id
        assert_eq!(parsed.skipped, 5);
        let rel = parsed.elements.iter().find(|e| e.osm_id == 3001).unwrap();
        assert_eq!(rel.osm_type, "relation");
        assert!((rel.lat - 39.7440).abs() < 1e-9);
        assert_eq!(classify(&rel.tags), Category::Flock);
        let bare = parsed.elements.iter().find(|e| e.osm_id == 1005).unwrap();
        assert!(bare.tags.is_empty());
        assert_eq!(classify(&bare.tags), Category::Unknown);
    }

    #[test]
    fn zero_elements_is_success() {
        let parsed = parse_response(r#"{"version":0.6,"generator":"x","elements":[]}"#).unwrap();
        assert!(parsed.elements.is_empty());
        assert_eq!(parsed.skipped, 0);
    }

    #[test]
    fn invalid_json_is_parse_error() {
        let err = parse_response("<html>Gateway timeout</html>").unwrap_err();
        assert_eq!(err.kind(), "parse");
        let err = parse_response(r#"{"version":0.6}"#).unwrap_err();
        assert!(err.to_string().contains("elements"));
    }

    #[test]
    fn runtime_error_remark_is_rejected_even_with_partial_data() {
        let body = r#"{"version":0.6,"elements":[{"type":"node","id":1,"lat":1.0,"lon":1.0}],"remark":"runtime error: Query timed out in \"query\" at line 3 after 60 seconds."}"#;
        let err = parse_response(body).unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn non_string_tag_values_are_stringified() {
        let body = r#"{"elements":[{"type":"node","id":7,"lat":1.0,"lon":2.0,"tags":{"direction":90,"ref":"A"}}]}"#;
        let parsed = parse_response(body).unwrap();
        assert_eq!(parsed.elements[0].tags.get("direction").unwrap(), "90");
    }
}
