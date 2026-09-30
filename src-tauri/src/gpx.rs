//! GPX parsing: extracts an ordered (lat, lon) polyline from `<trkpt>` (or `<rtept>`)
//! elements with specific, named errors for the ways real files go wrong.

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GpxError {
    #[error("GPX file is not well-formed XML: {0}")]
    InvalidXml(String),
    #[error("file is XML but its root element is <{0}>, not <gpx>")]
    NotGpx(String),
    #[error("GPX file contains no track points (<trkpt>) or route points (<rtept>)")]
    NoTrackPoints,
    #[error("GPX file has {0} track points but none carry usable lat/lon attributes (timestamps only?)")]
    MissingCoordinates(usize),
    #[error("GPX file has only {0} usable point(s); a route needs at least 2")]
    TooFewPoints(usize),
}

impl From<GpxError> for crate::error::AppError {
    fn from(e: GpxError) -> Self {
        crate::error::AppError::Parse(e.to_string())
    }
}

fn local(name: &str) -> &str {
    match name.rfind(':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// Parse a GPX document into an ordered polyline of (lat, lon).
///
/// All `<trkpt>` elements are used in document order (segments are concatenated). If the
/// file has no track points at all, `<rtept>` elements are used instead.
pub fn parse_gpx(xml: &str) -> Result<Vec<(f64, f64)>, GpxError> {
    Ok(parse_points(xml)?.into_iter().map(|p| (p.lat, p.lon)).collect())
}

/// A track point with its recorded time, if it has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedPoint {
    pub lat: f64,
    pub lon: f64,
    /// `<time>`, milliseconds since the Unix epoch.
    pub time_ms: Option<i64>,
}

/// Like `parse_gpx`, keeping each point's `<time>` (for replaying a recorded drive).
pub fn parse_points(xml: &str) -> Result<Vec<TimedPoint>, GpxError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut root_seen = false;
    let mut depth = 0i64;
    let mut track: Vec<TimedPoint> = Vec::new();
    let mut route: Vec<TimedPoint> = Vec::new();
    let mut trkpt_total = 0usize;
    let mut rtept_total = 0usize;
    // Inside a point that was kept (track or route), and inside its <time>.
    let mut open: Option<bool> = None;
    let mut in_time = false;

    loop {
        let event = reader.read_event().map_err(|e| {
            GpxError::InvalidXml(format!("{e} at byte {}", reader.buffer_position()))
        })?;
        match event {
            Event::End(ref e) => {
                depth -= 1;
                let name = local(e.name().as_ref()).to_ascii_lowercase();
                if name == "time" {
                    in_time = false;
                } else if name == "trkpt" || name == "rtept" {
                    open = None;
                }
            }
            Event::Text(ref t) if in_time => {
                let Some(is_trk) = open else { continue };
                let text = t.xml10_content().into_owned();
                let time = chrono::DateTime::parse_from_rfc3339(text.trim()).ok().map(|d| d.timestamp_millis());
                let list = if is_trk { &mut track } else { &mut route };
                if let Some(p) = list.last_mut() {
                    p.time_ms = time;
                }
            }
            Event::Start(ref e) | Event::Empty(ref e) => {
                let starts = matches!(event, Event::Start(_));
                if starts {
                    depth += 1;
                }
                let name = local(e.name().as_ref()).to_ascii_lowercase();
                if !root_seen {
                    root_seen = true;
                    if name != "gpx" {
                        return Err(GpxError::NotGpx(name));
                    }
                    continue;
                }
                if name == "time" && starts {
                    in_time = true;
                    continue;
                }
                let is_trk = name == "trkpt";
                let is_rte = name == "rtept";
                if !(is_trk || is_rte) {
                    continue;
                }
                if is_trk {
                    trkpt_total += 1;
                } else {
                    rtept_total += 1;
                }
                let mut lat = None;
                let mut lon = None;
                for attr in e.attributes().flatten() {
                    let key = local(attr.key.as_ref()).to_ascii_lowercase();
                    #[allow(deprecated)]
                    let val = attr.unescape_value().ok();
                    let parsed = val.and_then(|v| v.trim().parse::<f64>().ok());
                    if key == "lat" {
                        lat = parsed;
                    } else if key == "lon" {
                        lon = parsed;
                    }
                }
                if let (Some(lat), Some(lon)) = (lat, lon) {
                    if crate::geo_util::valid_coord(lat, lon) {
                        let p = TimedPoint { lat, lon, time_ms: None };
                        if is_trk {
                            track.push(p);
                        } else {
                            route.push(p);
                        }
                        if starts {
                            open = Some(is_trk);
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if !root_seen {
        return Err(GpxError::InvalidXml("document has no root element".into()));
    }
    if depth != 0 {
        return Err(GpxError::InvalidXml(format!(
            "unexpected end of file with {depth} unclosed element(s); the file is truncated"
        )));
    }
    let (points, total) = if trkpt_total > 0 {
        (track, trkpt_total)
    } else {
        (route, rtept_total)
    };
    if total == 0 {
        return Err(GpxError::NoTrackPoints);
    }
    if points.is_empty() {
        return Err(GpxError::MissingCoordinates(total));
    }
    if points.len() < 2 {
        return Err(GpxError::TooFewPoints(points.len()));
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
  <trk><name>Commute</name>
    <trkseg>
      <trkpt lat="39.7392" lon="-104.9903"><ele>1609</ele><time>2026-09-10T12:00:00Z</time></trkpt>
      <trkpt lat="39.7400" lon="-104.9850"/>
    </trkseg>
    <trkseg>
      <trkpt lat="39.7450" lon="-104.9800"><time>2026-09-10T12:05:00Z</time></trkpt>
    </trkseg>
  </trk>
</gpx>"#;

    #[test]
    fn parses_track_points_across_segments_in_order() {
        let pts = parse_gpx(GOOD).unwrap();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0], (39.7392, -104.9903));
        assert_eq!(pts[2], (39.7450, -104.9800));
    }

    #[test]
    fn falls_back_to_route_points() {
        let xml = r#"<gpx version="1.1"><rte><rtept lat="1" lon="2"/><rtept lat="1.1" lon="2.1"/></rte></gpx>"#;
        assert_eq!(parse_gpx(xml).unwrap(), vec![(1.0, 2.0), (1.1, 2.1)]);
    }

    #[test]
    fn corrupt_xml_names_the_problem() {
        let err = parse_gpx("<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\">").unwrap_err();
        assert!(matches!(err, GpxError::InvalidXml(_)), "{err:?}");
        let err = parse_gpx("").unwrap_err();
        assert!(matches!(err, GpxError::InvalidXml(_)), "{err:?}");
        let err = parse_gpx("not xml at all").unwrap_err();
        assert!(matches!(err, GpxError::InvalidXml(_)), "{err:?}");
    }

    #[test]
    fn non_gpx_root_is_rejected() {
        let err = parse_gpx(r#"<kml><Placemark/></kml>"#).unwrap_err();
        assert_eq!(err, GpxError::NotGpx("kml".into()));
    }

    #[test]
    fn empty_track_is_rejected() {
        let err = parse_gpx(r#"<gpx version="1.1"><trk><name>x</name><trkseg/></trk></gpx>"#).unwrap_err();
        assert_eq!(err, GpxError::NoTrackPoints);
        let err = parse_gpx(r#"<gpx><wpt lat="1" lon="2"/></gpx>"#).unwrap_err();
        assert_eq!(err, GpxError::NoTrackPoints);
    }

    #[test]
    fn timestamps_only_is_rejected_with_count() {
        let xml = r#"<gpx><trk><trkseg>
            <trkpt><time>2026-09-10T12:00:00Z</time></trkpt>
            <trkpt><time>2026-09-10T12:01:00Z</time></trkpt>
        </trkseg></trk></gpx>"#;
        assert_eq!(parse_gpx(xml).unwrap_err(), GpxError::MissingCoordinates(2));
    }

    #[test]
    fn out_of_range_points_are_dropped() {
        let xml = r#"<gpx><trk><trkseg>
            <trkpt lat="95" lon="0"/><trkpt lat="1" lon="2"/><trkpt lat="1.5" lon="2.5"/>
        </trkseg></trk></gpx>"#;
        assert_eq!(parse_gpx(xml).unwrap(), vec![(1.0, 2.0), (1.5, 2.5)]);
        let xml = r#"<gpx><trk><trkseg><trkpt lat="1" lon="2"/></trkseg></trk></gpx>"#;
        assert_eq!(parse_gpx(xml).unwrap_err(), GpxError::TooFewPoints(1));
    }

    #[test]
    fn handles_namespaced_elements() {
        let xml = r#"<g:gpx xmlns:g="http://www.topografix.com/GPX/1/1"><g:trk><g:trkseg>
            <g:trkpt lat="1" lon="2"/><g:trkpt lat="1.1" lon="2.2"/>
        </g:trkseg></g:trk></g:gpx>"#;
        assert_eq!(parse_gpx(xml).unwrap().len(), 2);
    }
}
