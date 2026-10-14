//! Points on the streets of Kyiv, for demo orders to lie on (D87).
//!
//! `scripts/streets.mjs` takes them out of the extract of OpenStreetMap that the OSRM
//! server is prepared from: a point every 25 metres along each street a car may stop in
//! for a parcel. They are compiled in, so a demonstration needs nothing beside the API,
//! as the approved description asks.
//!
//! The data is © OpenStreetMap contributors, under the Open Database License.

use routeforge_domain::{LatLon, haversine_m};

/// The south-west corner of the streets, as two little-endian `i32` in
/// hundred-thousandths of a degree, latitude first. Then every point, as two
/// little-endian `u16` north and east of that corner, in the same unit.
static FILE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/kyiv-streets.bin"
));

/// The unit of the file, a hundred-thousandth of a degree.
const STEPS_A_DEGREE: f64 = 1e5;

/// The points of the streets within `radius_m` of `centre`, in the order of the file.
/// None where the file has no streets.
pub(crate) fn near(centre: LatLon, radius_m: f64) -> Vec<LatLon> {
    let Some((&[s0, s1, s2, s3, w0, w1, w2, w3], points)) = FILE.split_first_chunk::<8>() else {
        return Vec::new();
    };
    let south = f64::from(i32::from_le_bytes([s0, s1, s2, s3]));
    let west = f64::from(i32::from_le_bytes([w0, w1, w2, w3]));
    let degrees = |corner: f64, steps: [u8; 2]| {
        (corner + f64::from(u16::from_le_bytes(steps))) / STEPS_A_DEGREE
    };

    let (points, _) = points.as_chunks::<4>();
    points
        .iter()
        .filter_map(|&[n0, n1, e0, e1]| {
            LatLon::new(degrees(south, [n0, n1]), degrees(west, [e0, e1])).ok()
        })
        .filter(|&point| haversine_m(centre, point) <= radius_m)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kyiv() -> LatLon {
        LatLon::new(50.45, 30.52).unwrap()
    }

    #[test]
    fn the_file_holds_the_streets_of_kyiv() {
        // A corner, and four bytes a point with nothing left over.
        assert_eq!((FILE.len() - 8) % 4, 0);
        // The extract is some 36 km from south to north and 41 from west to east.
        let all = near(kyiv(), 50_000.0);
        assert_eq!(all.len(), (FILE.len() - 8) / 4);
        assert!(all.len() > 100_000, "{}", all.len());
        assert!(all.iter().all(|point| {
            (50.29..50.63).contains(&point.lat()) && (30.27..30.87).contains(&point.lon())
        }));
    }

    #[test]
    fn the_points_within_a_radius_are_all_there_and_no_others() {
        let within = near(kyiv(), 10_000.0);
        assert!(within.len() > 50_000, "{}", within.len());
        assert!(
            within
                .iter()
                .all(|&point| haversine_m(kyiv(), point) <= 10_000.0)
        );
        let wider = near(kyiv(), 20_000.0);
        let of_them = wider
            .iter()
            .filter(|&&point| haversine_m(kyiv(), point) <= 10_000.0)
            .count();
        assert_eq!(of_them, within.len());

        // Far from Kyiv there is nothing.
        let lviv = LatLon::new(49.84, 24.03).unwrap();
        assert_eq!(near(lviv, 10_000.0), Vec::new());
    }
}
