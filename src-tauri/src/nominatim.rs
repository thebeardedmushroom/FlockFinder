//! Nominatim geocoding (forward search + reverse lookup), rate limited to 1 req/s.

use crate::error::{AppError, AppResult};
use crate::grid::BBox;
use crate::http::HttpClient;
use serde::{Deserialize, Serialize};

pub const ENDPOINT: &str = "https://nominatim.openstreetmap.org";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeocodeResult {
    pub display_name: String,
    pub lat: f64,
    pub lon: f64,
    pub bbox: Option<BBox>,
    pub osm_type: Option<String>,
    pub osm_id: Option<i64>,
}

fn num(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn parse_search(body: &str) -> AppResult<Vec<GeocodeResult>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("Nominatim response is not valid JSON: {e}")))?;
    let items = value
        .as_array()
        .ok_or_else(|| AppError::Parse("Nominatim response is not an array".into()))?;
    let mut out = Vec::new();
    for item in items {
        let (Some(lat), Some(lon)) = (num(item.get("lat")), num(item.get("lon"))) else {
            continue;
        };
        let bbox = item
            .get("boundingbox")
            .and_then(|b| b.as_array())
            .filter(|b| b.len() == 4)
            .and_then(|b| {
                Some(BBox::new(
                    num(b.first())?,
                    num(b.get(2))?,
                    num(b.get(1))?,
                    num(b.get(3))?,
                ))
            });
        out.push(GeocodeResult {
            display_name: item
                .get("display_name")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string(),
            lat,
            lon,
            bbox,
            osm_type: item.get("osm_type").and_then(|t| t.as_str()).map(String::from),
            osm_id: item.get("osm_id").and_then(|i| i.as_i64()),
        });
    }
    Ok(out)
}

pub async fn search(http: &HttpClient, query: &str) -> AppResult<Vec<GeocodeResult>> {
    http.nominatim_slot().await;
    let url = format!("{ENDPOINT}/search");
    let resp = http
        .send_with_backoff("Nominatim", || {
            http.client
                .get(&url)
                .query(&[("q", query), ("format", "jsonv2"), ("limit", "6")])
        })
        .await?;
    parse_search(&resp.text().await?)
}

/// Reverse geocode. `Ok(None)` means Nominatim knows nothing near the point
/// (its "Unable to geocode" answer), which is what open ocean looks like.
pub async fn reverse(http: &HttpClient, lat: f64, lon: f64) -> AppResult<Option<String>> {
    http.nominatim_slot().await;
    let url = format!("{ENDPOINT}/reverse");
    let lat_s = format!("{lat:.6}");
    let lon_s = format!("{lon:.6}");
    let resp = http
        .send_with_backoff("Nominatim", || {
            http.client.get(&url).query(&[
                ("lat", lat_s.as_str()),
                ("lon", lon_s.as_str()),
                ("format", "jsonv2"),
                ("zoom", "14"),
            ])
        })
        .await?;
    let body = resp.text().await?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| AppError::Parse(format!("Nominatim reverse response invalid: {e}")))?;
    if value.get("error").is_some() {
        return Ok(None);
    }
    Ok(value
        .get("display_name")
        .and_then(|d| d.as_str())
        .map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_coordinates_and_bbox() {
        let body = r#"[{"place_id":1,"osm_type":"relation","osm_id":112,"lat":"39.7392364","lon":"-104.984862","display_name":"Denver, Colorado, United States","boundingbox":["39.6143154","39.9142087","-105.1099","-104.5996"]}]"#;
        let r = parse_search(body).unwrap();
        assert_eq!(r.len(), 1);
        assert!((r[0].lat - 39.7392364).abs() < 1e-9);
        assert_eq!(r[0].osm_id, Some(112));
        let b = r[0].bbox.unwrap();
        assert!((b.south - 39.6143154).abs() < 1e-9);
        assert!((b.north - 39.9142087).abs() < 1e-9);
        assert!((b.west + 105.1099).abs() < 1e-9);
        assert!((b.east + 104.5996).abs() < 1e-9);
    }

    #[test]
    fn empty_array_and_bad_items() {
        assert!(parse_search("[]").unwrap().is_empty());
        let r = parse_search(r#"[{"display_name":"no coords"}]"#).unwrap();
        assert!(r.is_empty());
        assert!(parse_search("{}").is_err());
    }
}
