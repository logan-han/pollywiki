//! Electorate maps from the ABS's yearly Commonwealth Electoral Division
//! set: a mesh-block approximation of the AEC's boundaries, published under
//! CC BY 4.0 (the AEC's own maps are not openly licensed). Each division gets
//! one small SVG map, its own shape over the divisions around it, projected,
//! clipped and simplified here so the site only draws it.

use crate::endpoints::Endpoints;
use crate::http::fetch_json;
use crate::store::Store;
use anyhow::{bail, Result};
use pollywiki_schema::{slugify, Boundary, BoundaryNeighbour};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PREFIX: &str = "canonical/boundaries/";
const SET_KEY: &str = "canonical/boundary-set.json";
/// The server-side generalisation, in degrees (about 50 m).
const OFFSET: &str = "0.0005";
/// The map's long side in view units.
const SIZE: f64 = 1000.0;
/// Simplification tolerance in view units: finer than a stroke can show.
const TOLERANCE: f64 = 1.0;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoundarySet {
    set: String,
}

struct Shape {
    name: String,
    slug: String,
    /// Rings in longitude and latitude; holes are rings too, filled even-odd.
    rings: Vec<Vec<(f64, f64)>>,
    bbox: (f64, f64, f64, f64),
}

pub async fn sync_boundaries(store: &Store, endpoints: &Endpoints) -> Result<()> {
    let (set, layer, field) = find_set(endpoints).await?;
    let current = store.get_json::<BoundarySet>(SET_KEY).await?;
    if current.is_some_and(|c| c.set == set) && !store.list(PREFIX).await?.is_empty() {
        println!("boundaries: {set} already drawn");
        return Ok(());
    }
    let geojson: Value = fetch_json(
        &format!(
            "{layer}/query?where=1%3D1&outFields={field}&outSR=4326\
             &maxAllowableOffset={OFFSET}&geometryPrecision=5&f=geojson"
        ),
        &endpoints.opts(1000),
    )
    .await?;
    let shapes = shapes(&geojson, &field);
    if shapes.is_empty() {
        bail!("boundaries: {set} has no division shapes");
    }
    for (i, shape) in shapes.iter().enumerate() {
        store
            .put_json(
                &format!("{PREFIX}{}.json", shape.slug),
                &draw(&shapes, i, &set),
            )
            .await?;
    }
    store
        .put_json(SET_KEY, &BoundarySet { set: set.clone() })
        .await?;
    println!("boundaries: {} divisions drawn from {set}", shapes.len());
    Ok(())
}

/// The newest ASGS year with a division layer, its name field carrying the
/// year ("ced_name_2025"). A year the ABS has not yet published answers with
/// an error or no such field, and the search moves back a year.
async fn find_set(endpoints: &Endpoints) -> Result<(String, String, String)> {
    let this_year = chrono::Datelike::year(&chrono::Utc::now());
    for year in (2021..=this_year + 1).rev() {
        let layer = format!("{}/ASGS{year}/CED/FeatureServer/0", endpoints.abs_geo);
        let Ok(meta) = fetch_json::<Value>(&format!("{layer}?f=json"), &endpoints.opts(500)).await
        else {
            continue;
        };
        let field = meta["fields"].as_array().and_then(|fields| {
            fields
                .iter()
                .filter_map(|f| f["name"].as_str())
                .find(|n| n.starts_with("ced_name_"))
                .map(str::to_string)
        });
        if let Some(field) = field {
            return Ok((format!("ASGS {year}"), layer, field));
        }
    }
    bail!("boundaries: no ABS electoral division layer found")
}

fn shapes(geojson: &Value, field: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    for feature in geojson["features"].as_array().into_iter().flatten() {
        let Some(name) = feature["properties"][field].as_str() else {
            continue;
        };
        // The set's pseudo-divisions ("No usual address (NSW)") have no shape.
        let geometry = &feature["geometry"];
        let polygons: Vec<&Value> = match geometry["type"].as_str() {
            Some("Polygon") => vec![&geometry["coordinates"]],
            Some("MultiPolygon") => geometry["coordinates"]
                .as_array()
                .into_iter()
                .flatten()
                .collect(),
            _ => continue,
        };
        let rings: Vec<Vec<(f64, f64)>> = polygons
            .iter()
            .flat_map(|p| p.as_array().into_iter().flatten())
            .map(|ring| {
                ring.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|pt| Some((pt[0].as_f64()?, pt[1].as_f64()?)))
                    .collect::<Vec<_>>()
            })
            .filter(|r| r.len() >= 3)
            .collect();
        if rings.is_empty() {
            continue;
        }
        let bbox = bounds(rings.iter().flatten().copied());
        out.push(Shape {
            name: name.to_string(),
            slug: slugify(name),
            rings,
            bbox,
        });
    }
    out
}

fn bounds(points: impl Iterator<Item = (f64, f64)>) -> (f64, f64, f64, f64) {
    points.fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), (x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    )
}

/// One division's map: its shape filled, the divisions in view outlined and
/// named where their visible part has room for it.
fn draw(shapes: &[Shape], target: usize, set: &str) -> Boundary {
    let own = &shapes[target];
    let (lon0, lat0, lon1, lat1) = main_extent(own);
    // Equirectangular around the division's middle latitude, north up.
    let k = ((lat0 + lat1) / 2.0).to_radians().cos();
    let project = |(lon, lat): (f64, f64)| (lon * k, -lat);
    let (x0, y0) = project((lon0, lat1));
    let (x1, y1) = project((lon1, lat0));
    // Padding around the division, and an aspect kept between 3:5 and 5:3
    // so a long thin seat does not draw as a sliver.
    let (mut w, mut h) = (x1 - x0, y1 - y0);
    let pad = w.max(h) * 0.12;
    w += 2.0 * pad;
    h += 2.0 * pad;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    if w < h * 0.6 {
        w = h * 0.6;
    } else if h < w * 0.6 {
        h = w * 0.6;
    }
    let scale = SIZE / w.max(h);
    let (vw, vh) = ((w * scale).round(), (h * scale).round());
    let (left, top) = (cx - w / 2.0, cy - h / 2.0);
    let to_view = |p: (f64, f64)| {
        let (x, y) = project(p);
        ((x - left) * scale, (y - top) * scale)
    };
    // The view back in degrees, for picking the shapes that reach it.
    let view_lon = (left / k, (left + w) / k);
    let view_lat = (-(top + h), -top);
    // Clip a fifth beyond the view, so the cut edges fall outside the frame.
    let margin = SIZE * 0.2;
    let frame = (-margin, -margin, vw + margin, vh + margin);

    let path_of = |shape: &Shape| -> (String, Vec<Vec<(f64, f64)>>) {
        let rings: Vec<Vec<(f64, f64)>> = shape
            .rings
            .iter()
            .map(|ring| ring.iter().copied().map(to_view).collect::<Vec<_>>())
            .map(|ring| clip(&ring, frame))
            .map(|ring| simplify(&ring, TOLERANCE))
            .filter(|ring| {
                let (a, b, c, d) = bounds(ring.iter().copied());
                ring.len() >= 3 && (c - a).max(d - b) >= 2.0
            })
            .collect();
        (svg_path(&rings), rings)
    };

    let (path, _) = path_of(own);
    let mut neighbours = Vec::new();
    for (i, shape) in shapes.iter().enumerate() {
        let (a, b, c, d) = shape.bbox;
        if i == target || c < view_lon.0 || a > view_lon.1 || d < view_lat.0 || b > view_lat.1 {
            continue;
        }
        let (path, rings) = path_of(shape);
        if path.is_empty() {
            continue;
        }
        neighbours.push(BoundaryNeighbour {
            slug: shape.slug.clone(),
            name: shape.name.clone(),
            path,
            label: label_point(&rings, (vw, vh)),
        });
    }
    neighbours.sort_by(|a, b| a.slug.cmp(&b.slug));
    Boundary {
        set: set.to_string(),
        view_box: format!("0 0 {vw} {vh}"),
        path,
        neighbours,
    }
}

/// The extent of the rings that make up nearly all of a division's area,
/// so remote specks (Lingiari's Cocos and Christmas Islands) do not shrink
/// the mainland to a corner of its own map. Specks outside it go undrawn.
fn main_extent(shape: &Shape) -> (f64, f64, f64, f64) {
    let mut rings: Vec<(&Vec<(f64, f64)>, f64)> =
        shape.rings.iter().map(|r| (r, area(r).abs())).collect();
    let total: f64 = rings.iter().map(|(_, a)| a).sum();
    rings.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut covered = 0.0;
    let mut kept = Vec::new();
    for (ring, a) in rings {
        kept.push(ring);
        covered += a;
        if covered >= total * 0.98 {
            break;
        }
    }
    bounds(kept.into_iter().flatten().copied())
}

/// Where a neighbour's name can sit: the middle of the biggest visible part
/// of it, when that point is inside the shape and clear of the frame's edge.
fn label_point(rings: &[Vec<(f64, f64)>], (vw, vh): (f64, f64)) -> Option<(i64, i64)> {
    let inset = SIZE * 0.06;
    let visible: Vec<Vec<(f64, f64)>> = rings
        .iter()
        .map(|r| clip(r, (0.0, 0.0, vw, vh)))
        .filter(|r| r.len() >= 3)
        .collect();
    let biggest = visible
        .iter()
        .max_by(|a, b| area(a).abs().total_cmp(&area(b).abs()))?;
    // Too small a part to carry a name.
    if area(biggest).abs() < vw * vh * 0.02 {
        return None;
    }
    let (x, y) = centroid(biggest)?;
    let clear = x > inset && x < vw - inset && y > inset && y < vh - inset;
    (clear && inside(biggest, (x, y))).then(|| (x.round() as i64, y.round() as i64))
}

fn area(ring: &[(f64, f64)]) -> f64 {
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
        .sum::<f64>()
        / 2.0
}

fn centroid(ring: &[(f64, f64)]) -> Option<(f64, f64)> {
    let a = area(ring);
    if a.abs() < f64::EPSILON {
        return None;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        let cross = p.0 * q.1 - q.0 * p.1;
        cx += (p.0 + q.0) * cross;
        cy += (p.1 + q.1) * cross;
    }
    Some((cx / (6.0 * a), cy / (6.0 * a)))
}

/// Even-odd ray casting.
fn inside(ring: &[(f64, f64)], (x, y): (f64, f64)) -> bool {
    let mut odd = false;
    for (p, q) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        if (p.1 > y) != (q.1 > y) && x < (q.0 - p.0) * (y - p.1) / (q.1 - p.1) + p.0 {
            odd = !odd;
        }
    }
    odd
}

/// One side of the clipping frame.
#[derive(Clone, Copy)]
enum Edge {
    Left(f64),
    Right(f64),
    Top(f64),
    Bottom(f64),
}

impl Edge {
    fn keeps(self, (x, y): (f64, f64)) -> bool {
        match self {
            Edge::Left(at) => x >= at,
            Edge::Right(at) => x <= at,
            Edge::Top(at) => y >= at,
            Edge::Bottom(at) => y <= at,
        }
    }

    /// Where the segment from a to b crosses this side.
    fn cut(self, a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        match self {
            Edge::Left(x) | Edge::Right(x) => {
                let t = (x - a.0) / (b.0 - a.0);
                (x, a.1 + t * (b.1 - a.1))
            }
            Edge::Top(y) | Edge::Bottom(y) => {
                let t = (y - a.1) / (b.1 - a.1);
                (a.0 + t * (b.0 - a.0), y)
            }
        }
    }
}

/// Sutherland-Hodgman against an axis-aligned frame.
fn clip(ring: &[(f64, f64)], (x0, y0, x1, y1): (f64, f64, f64, f64)) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = ring.to_vec();
    for edge in [
        Edge::Left(x0),
        Edge::Right(x1),
        Edge::Top(y0),
        Edge::Bottom(y1),
    ] {
        let input = std::mem::take(&mut out);
        let Some(&last) = input.last() else {
            break;
        };
        let mut prev = last;
        for &p in &input {
            match (edge.keeps(prev), edge.keeps(p)) {
                (true, true) => out.push(p),
                (true, false) => out.push(edge.cut(prev, p)),
                (false, true) => {
                    out.push(edge.cut(prev, p));
                    out.push(p);
                }
                (false, false) => {}
            }
            prev = p;
        }
    }
    out
}

/// Douglas-Peucker over a ring, keeping its first point.
fn simplify(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        let (mut far, mut far_at) = (0.0, 0);
        for i in start + 1..end {
            let d = distance(points[i], points[start], points[end]);
            if d > far {
                far = d;
                far_at = i;
            }
        }
        if far > tolerance {
            keep[far_at] = true;
            stack.push((start, far_at));
            stack.push((far_at, end));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

/// A point's distance from a segment, or from its start when it has no length.
fn distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    if length == 0.0 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Whole view units, each ring closed by Z rather than a repeated point.
fn svg_path(rings: &[Vec<(f64, f64)>]) -> String {
    let mut out = String::new();
    for ring in rings {
        let mut points: Vec<(i64, i64)> = Vec::with_capacity(ring.len());
        for &(x, y) in ring {
            let point = (x.round() as i64, y.round() as i64);
            if points.last() != Some(&point) {
                points.push(point);
            }
        }
        if points.len() > 1 && points.first() == points.last() {
            points.pop();
        }
        for (i, (x, y)) in points.iter().enumerate() {
            out.push(if i == 0 { 'M' } else { 'L' });
            out.push_str(&format!("{x} {y}"));
        }
        out.push('Z');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalStore;
    use crate::test_http::{Response, TestServer};
    use std::path::PathBuf;

    fn new_store(name: &str) -> Store {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/boundaries-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Store::Local(LocalStore::new(dir))
    }

    fn square(x: f64, y: f64, size: f64) -> serde_json::Value {
        serde_json::json!([[
            [x, y],
            [x + size, y],
            [x + size, y + size],
            [x, y + size],
            [x, y]
        ]])
    }

    /// Three divisions in a row near Melbourne, a far-off one, and one of
    /// the set's pseudo-divisions with no shape.
    fn geojson() -> String {
        serde_json::json!({ "type": "FeatureCollection", "features": [
            { "properties": { "ced_name_2025": "Sampleford" },
              "geometry": { "type": "Polygon", "coordinates": square(145.0, -37.9, 0.1) } },
            { "properties": { "ced_name_2025": "Oldbridge" },
              "geometry": { "type": "Polygon", "coordinates": square(145.1, -37.9, 0.1) } },
            { "properties": { "ced_name_2025": "Placeholder Bay" },
              "geometry": { "type": "MultiPolygon", "coordinates": [square(144.9, -37.9, 0.1), square(144.9, -38.0, 0.01)] } },
            { "properties": { "ced_name_2025": "Far Away" },
              "geometry": { "type": "Polygon", "coordinates": square(115.0, -32.0, 0.1) } },
            { "properties": { "ced_name_2025": "No usual address (Vic.)" }, "geometry": null }
        ] })
        .to_string()
    }

    #[tokio::test]
    async fn the_newest_published_set_is_drawn_once_per_division() {
        let this_year = chrono::Datelike::year(&chrono::Utc::now());
        let server = TestServer::start(move |req| {
            let year = this_year + 1;
            if req.path.contains(&format!("/ASGS{year}/")) {
                return Response::status(404, "not yet published");
            }
            if req.path.contains(&format!("/ASGS{this_year}/")) {
                // A folder with no division layer yet answers with an error body.
                return Response::json(r#"{"error":{"code":400}}"#);
            }
            if req.path.ends_with("FeatureServer/0?f=json") {
                return Response::json(
                    r#"{"fields":[{"name":"objectid"},{"name":"ced_name_2025"}]}"#,
                );
            }
            if req.path.contains("/query?") {
                assert!(req.path.contains("outFields=ced_name_2025"));
                assert!(req.path.contains("maxAllowableOffset=0.0005"));
                return Response::json(geojson());
            }
            Response::status(404, "unexpected path")
        });
        let store = new_store("draw");
        let endpoints = Endpoints::at(&server.base);
        sync_boundaries(&store, &endpoints).await.expect("sync");

        let map: Boundary = store
            .get_json("canonical/boundaries/sampleford.json")
            .await
            .unwrap()
            .expect("drawn");
        assert!(map.set.starts_with("ASGS "), "{}", map.set);
        assert!(map.path.starts_with('M') && map.path.ends_with('Z'));
        // A square keeps its four corners and nothing more.
        assert_eq!(map.path.matches('L').count(), 3, "{}", map.path);
        let names: Vec<&str> = map.neighbours.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["Oldbridge", "Placeholder Bay"],
            "only divisions in view"
        );
        // Seen only as slivers at the frame's edge, neither has room for a name.
        assert!(map.neighbours.iter().all(|n| n.label.is_none()));
        // The long side is the map's full size; north is up, so a square
        // degree draws taller than wide this far south.
        let parts: Vec<f64> = map
            .view_box
            .split(' ')
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(parts[3], SIZE);
        assert!(parts[2] < parts[3]);
        assert!(store
            .get_json::<Boundary>("canonical/boundaries/no-usual-address-vic.json")
            .await
            .unwrap()
            .is_none());

        // The same set is not fetched again.
        let hits = server.hits();
        sync_boundaries(&store, &endpoints).await.expect("again");
        assert_eq!(server.hits(), hits + 3, "three layer probes, no query");
    }

    #[tokio::test]
    async fn no_published_layer_is_a_failed_sync() {
        let server = TestServer::start(|_| Response::status(404, "nothing here"));
        let store = new_store("none");
        let err = sync_boundaries(&store, &Endpoints::at(&server.base))
            .await
            .expect_err("no layer");
        assert!(
            err.to_string().contains("no ABS electoral division layer"),
            "got {err}"
        );
    }

    #[test]
    fn simplification_keeps_corners_and_drops_points_on_a_straight_edge() {
        let ring = vec![
            (0.0, 0.0),
            (5.0, 0.1),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        assert_eq!(
            simplify(&ring, 1.0),
            vec![
                (0.0, 0.0),
                (10.0, 0.0),
                (10.0, 10.0),
                (0.0, 10.0),
                (0.0, 0.0)
            ]
        );
    }

    #[test]
    fn clipping_cuts_a_ring_at_the_frame() {
        let ring = vec![(-5.0, -5.0), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)];
        let clipped = clip(&ring, (0.0, 0.0, 10.0, 10.0));
        let (a, b, c, d) = bounds(clipped.iter().copied());
        assert_eq!((a, b, c, d), (0.0, 0.0, 5.0, 5.0));
        assert!((area(&clipped).abs() - 25.0).abs() < 1e-9);
        assert!(clip(&ring, (20.0, 20.0, 30.0, 30.0)).is_empty());
    }

    #[test]
    fn a_label_sits_inside_its_shape_or_not_at_all() {
        // An L whose centroid falls in the notch, outside the shape.
        let l_shape = vec![
            (100.0, 100.0),
            (900.0, 100.0),
            (900.0, 300.0),
            (300.0, 300.0),
            (300.0, 900.0),
            (100.0, 900.0),
        ];
        assert!(label_point(&[l_shape], (1000.0, 1000.0)).is_none());
        let square = vec![
            (100.0, 100.0),
            (900.0, 100.0),
            (900.0, 900.0),
            (100.0, 900.0),
        ];
        assert_eq!(label_point(&[square], (1000.0, 1000.0)), Some((500, 500)));
        let speck = vec![(10.0, 10.0), (20.0, 10.0), (20.0, 20.0), (10.0, 20.0)];
        assert!(
            label_point(&[speck], (1000.0, 1000.0)).is_none(),
            "too small to name"
        );
    }

    #[test]
    fn paths_use_whole_units_and_skip_repeated_points() {
        let path = svg_path(&[vec![(0.2, 0.2), (0.4, 0.1), (10.6, 0.0), (10.0, 10.0)]]);
        assert_eq!(path, "M0 0L11 0L10 10Z");
    }
}

#[cfg(test)]
mod extent_tests {
    use super::*;

    #[test]
    fn a_remote_speck_does_not_set_the_frame() {
        let mainland = vec![
            (130.0, -20.0),
            (138.0, -20.0),
            (138.0, -11.0),
            (130.0, -11.0),
        ];
        let island = vec![(96.8, -12.2), (96.9, -12.2), (96.9, -12.1), (96.8, -12.1)];
        let shape = Shape {
            name: "Lingiari".to_string(),
            slug: "lingiari".to_string(),
            bbox: bounds(mainland.iter().chain(&island).copied()),
            rings: vec![island, mainland],
        };
        assert_eq!(main_extent(&shape), (130.0, -20.0, 138.0, -11.0));
    }
}
