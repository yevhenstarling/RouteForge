//! Positions on Earth, the distances between them, a flat map for clustering (D29),
//! and a line through positions as text (D73).

use std::convert::Infallible;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::text::text_enum;

/// The mean radius of the Earth in metres, as the IUGG defines it.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

text_enum! {
    /// What a distance was measured along (D66).
    Distances {
        /// Great circles from stop to stop: as the crow flies.
        GreatCircle => "great_circle",
        /// Roads, as a routing service knows them.
        Road => "road",
    }
}

/// A position on Earth in degrees.
///
/// The latitude is between -90 and 90 and the longitude between -180 and 180. A value
/// of this type holds nothing else, NaN included: [`LatLon::new`] and deserialization
/// both check.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(try_from = "Unchecked")]
pub struct LatLon {
    /// Degrees north of the equator.
    #[cfg_attr(feature = "openapi", schema(minimum = -90.0, maximum = 90.0))]
    lat: f64,
    /// Degrees east of Greenwich.
    #[cfg_attr(feature = "openapi", schema(minimum = -180.0, maximum = 180.0))]
    lon: f64,
}

/// A latitude and longitude that are not a position on Earth.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error(
    "latitude {lat} and longitude {lon} are not a position: \
     latitude runs from -90 to 90 and longitude from -180 to 180"
)]
pub struct InvalidLatLon {
    pub lat: f64,
    pub lon: f64,
}

/// What a position looks like in JSON, before it is checked.
#[derive(Deserialize)]
struct Unchecked {
    lat: f64,
    lon: f64,
}

impl TryFrom<Unchecked> for LatLon {
    type Error = InvalidLatLon;

    fn try_from(unchecked: Unchecked) -> Result<Self, Self::Error> {
        Self::new(unchecked.lat, unchecked.lon)
    }
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Result<Self, InvalidLatLon> {
        // A NaN is in no range, so it is refused here as well.
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            Ok(Self { lat, lon })
        } else {
            Err(InvalidLatLon { lat, lon })
        }
    }

    /// Degrees north of the equator.
    pub fn lat(self) -> f64 {
        self.lat
    }

    /// Degrees east of Greenwich.
    pub fn lon(self) -> f64 {
        self.lon
    }
}

/// The great-circle distance between two positions in metres.
///
/// The Earth is taken as a sphere, which is within about 0.5 % of the real thing. The
/// result is the same in both directions, to the last bit.
///
/// ```
/// use routeforge_domain::{LatLon, haversine_m};
///
/// let south = LatLon::new(50.0, 30.0)?;
/// let north = LatLon::new(51.0, 30.0)?;
/// // A degree of latitude is 111.2 km everywhere.
/// assert_eq!(haversine_m(south, north).round(), 111_195.0);
/// # Ok::<(), routeforge_domain::InvalidLatLon>(())
/// ```
pub fn haversine_m(from: LatLon, to: LatLon) -> f64 {
    let (from_lat, to_lat) = (from.lat.to_radians(), to.lat.to_radians());
    let half_dlat = (to_lat - from_lat) / 2.0;
    let half_dlon = (to.lon - from.lon).to_radians() / 2.0;
    let haversine =
        half_dlat.sin().powi(2) + from_lat.cos() * to_lat.cos() * half_dlon.sin().powi(2);
    // Rounding can push the root a hair above 1 for opposite points, where asin is NaN.
    2.0 * EARTH_RADIUS_M * haversine.sqrt().min(1.0).asin()
}

/// A point on a [`Projection`], in metres east and north of its origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Planar {
    pub x: f64,
    pub y: f64,
}

/// A flat map of the surroundings of one position.
///
/// Sweep clustering needs the direction of every stop from the depot and a rough
/// distance, not road lengths. An equirectangular projection round the depot gives
/// both: across a city it is off by a fraction of a percent. It is not meant for the
/// poles, where every direction is south.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    origin: LatLon,
    /// How much shorter a degree of longitude is at the origin than at the equator.
    east_scale: f64,
}

impl Projection {
    pub fn centered_on(origin: LatLon) -> Self {
        Self {
            origin,
            east_scale: origin.lat.to_radians().cos(),
        }
    }

    pub fn project(&self, position: LatLon) -> Planar {
        // The short way round, so that a depot near the date line sees both sides of it.
        let dlon = (position.lon - self.origin.lon + 540.0).rem_euclid(360.0) - 180.0;
        let dlat = position.lat - self.origin.lat;
        Planar {
            x: EARTH_RADIUS_M * dlon.to_radians() * self.east_scale,
            y: EARTH_RADIUS_M * dlat.to_radians(),
        }
    }
}

/// The distance from each of a list of positions to each other, in whole metres (D4).
///
/// The table is directed: the way from A to B may differ from the way back, as it does
/// on roads. [`DistanceTable::haversine`] gives a symmetric one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistanceTable {
    nodes: usize,
    /// Row-major, `nodes * nodes` entries.
    metres: Vec<u32>,
}

impl DistanceTable {
    /// Wraps a row-major table. `None` if it does not hold `nodes * nodes` distances.
    pub fn new(nodes: usize, metres: Vec<u32>) -> Option<Self> {
        (nodes.checked_mul(nodes) == Some(metres.len())).then_some(Self { nodes, metres })
    }

    /// Great-circle distances, rounded to the nearest metre.
    pub fn haversine(positions: &[LatLon]) -> Self {
        let nodes = positions.len();
        let mut metres = vec![0; nodes * nodes];
        for (from, &start) in positions.iter().enumerate() {
            for (to, &end) in positions.iter().enumerate().skip(from + 1) {
                // No two positions are further apart than half the equator, 20 000 km,
                // which a `u32` holds two hundred times over.
                let between = haversine_m(start, end).round() as u32;
                metres[from * nodes + to] = between;
                metres[to * nodes + from] = between;
            }
        }
        Self { nodes, metres }
    }

    /// The number of positions.
    pub fn nodes(&self) -> usize {
        self.nodes
    }

    /// The distance from one position to another in metres.
    ///
    /// # Panics
    ///
    /// If a position is not below [`nodes`](Self::nodes), like indexing a slice.
    pub fn get(&self, from: usize, to: usize) -> u32 {
        assert!(
            to < self.nodes,
            "position {to} is outside a table of {} positions",
            self.nodes
        );
        self.metres[from * self.nodes + to]
    }

    /// The row-major table itself.
    pub fn into_metres(self) -> Vec<u32> {
        self.metres
    }
}

/// How many steps of a [`Polyline`] make a degree: six decimal places, which on the
/// ground is a tenth of a metre.
const POLYLINE_STEPS: f64 = 1e6;

/// Where the letters of a [`Polyline`] begin in ASCII: a letter is 63 more than the six
/// bits it stands for.
const FIRST_LETTER: u8 = 63;

/// A line on the map as text: an encoded polyline with six decimal places (D73).
///
/// It is the format OSRM writes the way of a route in, and it is short. Each point is
/// written as how far it is from the point before, the latitude first, five bits to a
/// letter. So a point of a way along streets takes some four letters, where two
/// numbers in JSON take forty.
///
/// A value of this type decodes into two positions or more, and holds nothing else:
/// [`Polyline::new`] and deserialization both check.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(try_from = "String", into = "String")]
pub struct Polyline(String);

/// A text that is no line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidPolyline {
    /// A letter that no number of a line is written with.
    #[error("letter {at} of the line is none that a line is written with")]
    Letter { at: usize },
    /// The text ends in the middle of a number, or after a latitude that has no
    /// longitude.
    #[error("the line breaks off in the middle of a point")]
    BrokenOff,
    /// A number of more letters than the distance between two positions takes.
    #[error("the number at letter {at} of the line is further than any position is from another")]
    TooLarge { at: usize },
    #[error("point {point} of the line is not a position on Earth")]
    OffTheGlobe { point: usize },
    #[error("a line has two points at least, and this has {points}")]
    TooShort { points: usize },
}

impl Polyline {
    /// Takes `text` for a line, if it is one.
    pub fn new(text: impl Into<String>) -> Result<Self, InvalidPolyline> {
        let text = text.into();
        decode(&text, POLYLINE_STEPS)?;
        Ok(Self(text))
    }

    /// The line through `positions` in the order given, each rounded to six decimal
    /// places. `None` for fewer than two: a line has two ends.
    pub fn through(positions: &[LatLon]) -> Option<Self> {
        (positions.len() >= 2).then(|| Self(encode(positions, POLYLINE_STEPS)))
    }

    /// The positions the line goes through.
    pub fn positions(&self) -> Vec<LatLon> {
        // It was decoded when it was made, so it decodes.
        decode(&self.0, POLYLINE_STEPS).unwrap_or_default()
    }

    /// How long the line is in metres, along great circles from point to point.
    pub fn length_m(&self) -> f64 {
        let positions = self.positions();
        positions
            .windows(2)
            .map(|leg| haversine_m(leg[0], leg[1]))
            .sum()
    }

    /// The line as it is stored and sent.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Polyline {
    type Error = InvalidPolyline;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

impl From<Polyline> for String {
    fn from(line: Polyline) -> Self {
        line.0
    }
}

// Written by hand: the way of a route is tens of thousands of letters, which a log
// line or a failed assertion is no place for.
impl fmt::Debug for Polyline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 48;
        // Every letter of a line is one byte, so the text can be cut anywhere.
        match self.0.get(..SHOWN) {
            Some(start) if self.0.len() > SHOWN => {
                write!(f, "Polyline({start:?}, and {} more)", self.0.len() - SHOWN)
            }
            _ => write!(f, "Polyline({:?})", self.0),
        }
    }
}

/// The positions `text` is the line through, with `steps` steps to a degree.
fn decode(text: &str, steps: f64) -> Result<Vec<LatLon>, InvalidPolyline> {
    let mut letters = text.bytes().enumerate();
    let mut positions = Vec::new();
    // In steps. A point is written as how far it is from the one before, and the
    // first as how far it is from nought.
    let (mut lat, mut lon) = (0_i64, 0_i64);
    while let Some(north) = number(&mut letters)? {
        let east = number(&mut letters)?.ok_or(InvalidPolyline::BrokenOff)?;
        // Neither can run over: a number is 35 bits at the most, and what it is added
        // to was a position a moment ago.
        lat += north;
        lon += east;
        let point = positions.len();
        let position = LatLon::new(lat as f64 / steps, lon as f64 / steps)
            .map_err(|_| InvalidPolyline::OffTheGlobe { point })?;
        positions.push(position);
    }
    if positions.len() < 2 {
        return Err(InvalidPolyline::TooShort {
            points: positions.len(),
        });
    }
    Ok(positions)
}

/// The next number of a line, or `None` where the text ends.
///
/// A number is written five bits to a letter, the lowest first, and every letter but
/// the last has a sixth bit set to say that more follow. The lowest bit of all is the
/// sign: a number below nought is written as its complement, which sets it.
fn number(letters: &mut impl Iterator<Item = (usize, u8)>) -> Result<Option<i64>, InvalidPolyline> {
    /// Seven letters. Once round the globe is 360 million steps, which takes 30 bits
    /// with the sign.
    const MOST_BITS: u32 = 35;

    let mut written = 0_i64;
    let mut bits = 0;
    loop {
        let Some((at, letter)) = letters.next() else {
            return if bits == 0 {
                Ok(None)
            } else {
                Err(InvalidPolyline::BrokenOff)
            };
        };
        let six = letter
            .checked_sub(FIRST_LETTER)
            .filter(|six| *six < 64)
            .ok_or(InvalidPolyline::Letter { at })?;
        if bits >= MOST_BITS {
            return Err(InvalidPolyline::TooLarge { at });
        }
        written |= i64::from(six & 0x1f) << bits;
        bits += 5;
        if six & 0x20 == 0 {
            break;
        }
    }
    Ok(Some(if written & 1 == 0 {
        written >> 1
    } else {
        !(written >> 1)
    }))
}

/// The line through `positions` as text, with `steps` steps to a degree.
fn encode(positions: &[LatLon], steps: f64) -> String {
    let mut text = String::new();
    let (mut lat, mut lon) = (0_i64, 0_i64);
    for position in positions {
        let north = (position.lat * steps).round() as i64;
        let east = (position.lon * steps).round() as i64;
        write_number(&mut text, north - lat);
        write_number(&mut text, east - lon);
        (lat, lon) = (north, east);
    }
    text
}

/// Writes one number of a line, as [`number`] reads it.
fn write_number(text: &mut String, number: i64) {
    let mut rest = if number < 0 {
        !(number << 1)
    } else {
        number << 1
    };
    loop {
        let five = (rest & 0x1f) as u8;
        rest >>= 5;
        let more = if rest == 0 { 0 } else { 0x20 };
        text.push(char::from(FIRST_LETTER + five + more));
        if rest == 0 {
            break;
        }
    }
}

/// A source of distances between positions.
///
/// [`Haversine`] is the default and needs nothing outside the process (D8). A provider
/// that asks a routing service does I/O, which is why the methods return a future, and
/// can fail, which is why the error is its own.
pub trait DistanceProvider {
    type Error: std::error::Error + Send + Sync + 'static;

    /// What the distances of this provider are measured along. A route says it of its
    /// length, so that a length by road is not taken for one as the crow flies (D66).
    fn measures(&self) -> Distances;

    /// The distance from each position to each other, in the order they were given.
    fn table(
        &self,
        positions: &[LatLon],
    ) -> impl Future<Output = Result<DistanceTable, Self::Error>> + Send;

    /// The way from the first of `positions` through the others in the order given,
    /// along what this provider measures, as a line for a map (D73).
    ///
    /// `None` from a provider that measures along straight lines. The line from each
    /// position to the next is the way then, and whoever draws it needs no telling.
    fn path(
        &self,
        positions: &[LatLon],
    ) -> impl Future<Output = Result<Option<Polyline>, Self::Error>> + Send;
}

/// Great-circle distances. Works offline and never fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Haversine;

impl DistanceProvider for Haversine {
    type Error = Infallible;

    fn measures(&self) -> Distances {
        Distances::GreatCircle
    }

    async fn table(&self, positions: &[LatLon]) -> Result<DistanceTable, Infallible> {
        Ok(DistanceTable::haversine(positions))
    }

    async fn path(&self, _positions: &[LatLon]) -> Result<Option<Polyline>, Infallible> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use proptest::prelude::*;

    use super::*;

    fn at(lat: f64, lon: f64) -> LatLon {
        LatLon::new(lat, lon).unwrap()
    }

    fn position() -> impl Strategy<Value = LatLon> {
        (-90.0..=90.0f64, -180.0..=180.0f64).prop_map(|(lat, lon)| at(lat, lon))
    }

    /// The same distance by the spherical law of cosines. It shares no step with the
    /// haversine formula, and is only trusted to a metre: it loses digits when the two
    /// positions are close together or nearly opposite.
    fn by_law_of_cosines(from: LatLon, to: LatLon) -> f64 {
        let (from_lat, to_lat) = (from.lat().to_radians(), to.lat().to_radians());
        let dlon = (to.lon() - from.lon()).to_radians();
        let cosine = from_lat.sin() * to_lat.sin() + from_lat.cos() * to_lat.cos() * dlon.cos();
        EARTH_RADIUS_M * cosine.clamp(-1.0, 1.0).acos()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0),
            "{actual} is not {expected}"
        );
    }

    #[test]
    fn positions_outside_the_globe_are_refused() {
        assert!(LatLon::new(90.0, 180.0).is_ok());
        assert!(LatLon::new(-90.0, -180.0).is_ok());
        for (lat, lon) in [
            (90.1, 0.0),
            (-90.1, 0.0),
            (0.0, 180.1),
            (0.0, -180.1),
            (f64::NAN, 0.0),
            (0.0, f64::NAN),
            (f64::INFINITY, 0.0),
            (0.0, f64::NEG_INFINITY),
        ] {
            let error = LatLon::new(lat, lon).unwrap_err();
            assert!(error.to_string().contains("not a position"), "{lat}, {lon}");
        }
    }

    #[test]
    fn json_cannot_smuggle_in_a_bad_position() {
        let position: LatLon = serde_json::from_str(r#"{"lat": 50.45, "lon": 30.52}"#).unwrap();
        assert_eq!(position, at(50.45, 30.52));
        assert_eq!(
            serde_json::to_value(position).unwrap(),
            serde_json::json!({ "lat": 50.45, "lon": 30.52 })
        );

        let error = serde_json::from_str::<LatLon>(r#"{"lat": 95.0, "lon": 30.52}"#).unwrap_err();
        assert!(error.to_string().contains("not a position"));
        assert!(serde_json::from_str::<LatLon>(r#"{"lat": 50.45}"#).is_err());
    }

    /// Arcs whose length follows from the radius alone.
    #[test]
    fn known_arcs_have_their_length() {
        // A degree along a meridian and along the equator.
        assert_close(
            haversine_m(at(10.0, 20.0), at(11.0, 20.0)),
            EARTH_RADIUS_M * PI / 180.0,
        );
        assert_close(
            haversine_m(at(0.0, 20.0), at(0.0, 21.0)),
            EARTH_RADIUS_M * PI / 180.0,
        );
        // From the equator to a pole.
        assert_close(
            haversine_m(at(0.0, 77.0), at(90.0, 0.0)),
            EARTH_RADIUS_M * PI / 2.0,
        );
        // Opposite sides of the 60th parallel: over the pole, 30 degrees on each side.
        assert_close(
            haversine_m(at(60.0, 0.0), at(60.0, 180.0)),
            EARTH_RADIUS_M * PI / 3.0,
        );
        // Opposite points of the globe, where rounding must not produce a NaN.
        assert_close(
            haversine_m(at(0.0, 0.0), at(0.0, 180.0)),
            EARTH_RADIUS_M * PI,
        );
        assert_close(
            haversine_m(at(-35.0, 100.0), at(35.0, -80.0)),
            EARTH_RADIUS_M * PI,
        );
        // Across the date line the short way.
        assert_close(
            haversine_m(at(0.0, 179.5), at(0.0, -179.5)),
            EARTH_RADIUS_M * PI / 180.0,
        );
    }

    proptest! {
        #[test]
        fn the_distance_is_the_same_both_ways(a in position(), b in position()) {
            prop_assert_eq!(haversine_m(a, b).to_bits(), haversine_m(b, a).to_bits());
        }

        #[test]
        fn the_distance_is_a_real_length(a in position(), b in position()) {
            let metres = haversine_m(a, b);
            prop_assert!((0.0..=EARTH_RADIUS_M * PI).contains(&metres));
            prop_assert_eq!(haversine_m(a, a), 0.0);
        }

        #[test]
        fn another_formula_gives_the_same_distance(a in position(), b in position()) {
            prop_assert!((haversine_m(a, b) - by_law_of_cosines(a, b)).abs() < 1.0);
        }

        #[test]
        fn a_detour_is_never_shorter(a in position(), b in position(), c in position()) {
            // A metre of slack: near opposite points the last digits of an arc are noise.
            prop_assert!(haversine_m(a, c) <= haversine_m(a, b) + haversine_m(b, c) + 1.0);
        }
    }

    #[test]
    fn the_map_has_its_origin_in_the_middle_and_north_at_the_top() {
        let depot = at(50.45, 30.52);
        let map = Projection::centered_on(depot);
        assert_eq!(map.project(depot), Planar { x: 0.0, y: 0.0 });

        let north = map.project(at(50.46, 30.52));
        assert!(north.x == 0.0 && north.y > 0.0);
        let east = map.project(at(50.45, 30.53));
        assert!(east.x > 0.0 && east.y == 0.0);
        let south_west = map.project(at(50.44, 30.51));
        assert!(south_west.x < 0.0 && south_west.y < 0.0);

        // A hundredth of a degree is 1 112 m northwards and, at this latitude, 708 m eastwards.
        assert_close(north.y, EARTH_RADIUS_M * PI / 18_000.0);
        assert_close(east.x, north.y * 50.45f64.to_radians().cos());
    }

    #[test]
    fn the_map_takes_the_short_way_across_the_date_line() {
        let map = Projection::centered_on(at(-17.0, 179.95));
        let just_east = map.project(at(-17.0, -179.95));
        assert!(just_east.x > 0.0);
        assert_close(
            just_east.x,
            haversine_m(at(-17.0, 179.95), at(-17.0, -179.95)),
        );
    }

    /// A longitude a little beyond 180 comes back into range the way round the globe.
    fn wrapped(lon: f64) -> f64 {
        (lon + 180.0).rem_euclid(360.0) - 180.0
    }

    proptest! {
        /// Up to 0.3 degrees from the origin each way, which is 33 km to the north and
        /// to the south, and up to 60 degrees of latitude: a city and its surroundings,
        /// anywhere people live.
        #[test]
        fn distances_on_the_map_are_within_two_percent_of_the_globe(
            origin_lat in -60.0..=60.0f64,
            origin_lon in -180.0..=180.0f64,
            offsets in prop::array::uniform4(-0.3..=0.3f64),
        ) {
            let origin = at(origin_lat, origin_lon);
            let a = at(origin_lat + offsets[0], wrapped(origin_lon + offsets[1]));
            let b = at(origin_lat + offsets[2], wrapped(origin_lon + offsets[3]));

            let map = Projection::centered_on(origin);
            let (flat_a, flat_b) = (map.project(a), map.project(b));
            let on_the_map = (flat_a.x - flat_b.x).hypot(flat_a.y - flat_b.y);
            let on_the_globe = haversine_m(a, b);
            prop_assert!(
                (on_the_map - on_the_globe).abs() <= 0.02 * on_the_globe + 0.01,
                "{on_the_map} m on the map, {on_the_globe} m on the globe"
            );
        }
    }

    #[test]
    fn a_table_needs_a_distance_for_every_pair() {
        let table = DistanceTable::new(2, vec![0, 5, 7, 0]).unwrap();
        assert_eq!(table.nodes(), 2);
        // Directed: the way there and the way back are kept apart.
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
        assert_eq!(table.into_metres(), [0, 5, 7, 0]);

        assert_eq!(DistanceTable::new(2, vec![0; 3]), None);
        assert_eq!(DistanceTable::new(usize::MAX, Vec::new()), None);
        assert_eq!(DistanceTable::new(0, Vec::new()).unwrap().nodes(), 0);
    }

    #[test]
    #[should_panic(expected = "outside a table")]
    fn a_table_refuses_a_position_it_does_not_have() {
        DistanceTable::haversine(&[at(0.0, 0.0)]).get(0, 1);
    }

    proptest! {
        #[test]
        fn the_haversine_table_is_symmetric_and_rounds_to_metres(
            positions in prop::collection::vec(position(), 0..8),
        ) {
            let table = DistanceTable::haversine(&positions);
            prop_assert_eq!(table.nodes(), positions.len());
            for (from, &start) in positions.iter().enumerate() {
                prop_assert_eq!(table.get(from, from), 0);
                for (to, &end) in positions.iter().enumerate() {
                    prop_assert_eq!(table.get(from, to), table.get(to, from));
                    prop_assert_eq!(f64::from(table.get(from, to)), haversine_m(start, end).round());
                }
            }
        }
    }

    #[tokio::test]
    async fn the_default_provider_gives_the_haversine_table() {
        let positions = [at(50.45, 30.52), at(50.40, 30.60), at(50.50, 30.45)];
        let Ok(table) = Haversine.table(&positions).await;
        assert_eq!(table, DistanceTable::haversine(&positions));
        // Some 8 km between the first two.
        assert!((7_000..9_000).contains(&table.get(0, 1)));
        assert_eq!(Haversine.measures(), Distances::GreatCircle);
        // Between great circles the straight line is the way, and there is none to give.
        let Ok(path) = Haversine.path(&positions).await;
        assert_eq!(path, None);
    }

    /// The example every description of the format gives. It is written with five
    /// decimal places, and read here by the code that reads six.
    #[test]
    fn a_line_is_written_as_the_format_says() {
        let text = "_p~iF~ps|U_ulLnnqC_mqNvxq`@";
        let points = [at(38.5, -120.2), at(40.7, -120.95), at(43.252, -126.453)];
        assert_eq!(decode(text, 1e5).unwrap(), points);
        assert_eq!(encode(&points, 1e5), text);
    }

    #[test]
    fn a_line_has_six_decimal_places_and_the_latitude_first() {
        let depot = at(50.45, 30.52);
        let north_east = at(50.450_001, 30.520_002);
        let line = Polyline::through(&[depot, north_east]).unwrap();
        assert_eq!(line.positions(), [depot, north_east]);
        // One step north and two east: each a letter, the sign in its lowest bit.
        assert!(line.as_str().ends_with("AC"), "{line:?}");
        // The seventh place is rounded away.
        let finer = Polyline::through(&[at(50.450_000_4, 30.52), at(50.450_001_4, 30.520_002)]);
        assert_eq!(finer.unwrap(), line);

        // Back the way it came, which is below nought both ways.
        let back = Polyline::through(&[north_east, depot]).unwrap();
        assert_eq!(back.positions(), [north_east, depot]);
        assert!(back.as_str().ends_with("@B"), "{back:?}");

        // A degree of latitude is 111.2 km everywhere.
        let degree = Polyline::through(&[at(50.0, 30.0), at(51.0, 30.0)]).unwrap();
        assert_eq!(degree.length_m().round(), 111_195.0);
        // From pole to pole and half way round, which is the most a number is asked to say.
        let far = [at(-90.0, -180.0), at(90.0, 180.0), at(-90.0, -180.0)];
        assert_eq!(Polyline::through(&far).unwrap().positions(), far);
    }

    #[test]
    fn a_line_has_two_ends() {
        let depot = at(50.45, 30.52);
        assert_eq!(Polyline::through(&[]), None);
        assert_eq!(Polyline::through(&[depot]), None);
        // The same place twice is a line all the same, of no length: it is what a
        // routing service answers for a way from a place to itself.
        let nowhere = Polyline::through(&[depot, depot]).unwrap();
        assert!(nowhere.as_str().ends_with("??"), "{nowhere:?}");
        assert_eq!(nowhere.length_m(), 0.0);

        assert_eq!(
            Polyline::new(""),
            Err(InvalidPolyline::TooShort { points: 0 })
        );
        let one_point = encode(&[depot], POLYLINE_STEPS);
        assert_eq!(
            Polyline::new(one_point),
            Err(InvalidPolyline::TooShort { points: 1 })
        );
    }

    #[test]
    fn text_that_is_no_line_is_refused() {
        let line = Polyline::through(&[at(50.45, 30.52), at(50.46, 30.53)]).unwrap();
        let text = line.as_str();
        assert_eq!(Polyline::new(text).unwrap(), line);

        // Cut off in the middle of a number, and after a latitude.
        assert_eq!(
            Polyline::new(&text[..text.len() - 1]),
            Err(InvalidPolyline::BrokenOff)
        );
        // And in the middle of the latitude of a third point. The two points before it
        // are a line by themselves, and it is not that line that was sent.
        let third_begun = format!("{text}_");
        assert_eq!(Polyline::new(third_begun), Err(InvalidPolyline::BrokenOff));
        let latitude_alone = format!("{text}_ibE");
        assert_eq!(
            Polyline::new(latitude_alone),
            Err(InvalidPolyline::BrokenOff)
        );
        // A letter from outside the 64 a line is written with, on either side.
        for stranger in [' ', '>', '\u{7f}', 'é', '\n'] {
            let spoilt = format!("{text}{stranger}");
            assert_eq!(
                Polyline::new(spoilt),
                Err(InvalidPolyline::Letter { at: text.len() }),
                "{stranger:?}"
            );
        }
        // A number that goes on for ever.
        assert_eq!(
            Polyline::new("~~~~~~~~~~~~"),
            Err(InvalidPolyline::TooLarge { at: 7 })
        );
        // A point off the globe: thirty degrees north of the eightieth parallel.
        let mut off = encode(&[at(50.0, 30.0), at(80.0, 30.0)], POLYLINE_STEPS);
        write_number(&mut off, 30_000_000);
        write_number(&mut off, 0);
        assert_eq!(
            Polyline::new(off),
            Err(InvalidPolyline::OffTheGlobe { point: 2 })
        );
        // And what is plainly something else.
        assert!(Polyline::new("{\"type\": \"LineString\"}").is_err());
    }

    #[test]
    fn json_cannot_smuggle_in_what_is_no_line() {
        let line = Polyline::through(&[at(50.45, 30.52), at(50.46, 30.53)]).unwrap();
        let json = serde_json::to_value(&line).unwrap();
        assert_eq!(json, serde_json::json!(line.as_str()));
        assert_eq!(serde_json::from_value::<Polyline>(json).unwrap(), line);

        let error = serde_json::from_str::<Polyline>("\"not a line\"").unwrap_err();
        assert!(error.to_string().contains("line"), "{error}");
        assert!(serde_json::from_str::<Polyline>("[[50.45, 30.52]]").is_err());
    }

    #[test]
    fn a_long_line_is_not_written_out_where_it_is_shown() {
        let short = Polyline::through(&[at(50.45, 30.52), at(50.46, 30.53)]).unwrap();
        assert_eq!(
            format!("{short:?}"),
            format!("Polyline({:?})", short.as_str())
        );

        let stops: Vec<LatLon> = (0..200)
            .map(|step| at(50.0 + f64::from(step) * 0.001, 30.0))
            .collect();
        let long = Polyline::through(&stops).unwrap();
        let shown = format!("{long:?}");
        assert!(shown.len() < 100, "{shown}");
        assert!(shown.contains(&long.as_str()[..48]), "{shown}");
        assert!(shown.contains(&format!("{} more", long.as_str().len() - 48)));
    }

    proptest! {
        #[test]
        fn a_line_is_read_back_as_it_was_written(
            positions in prop::collection::vec(position(), 2..40),
        ) {
            let line = Polyline::through(&positions).unwrap();
            let read = line.positions();
            prop_assert_eq!(read.len(), positions.len());
            for (read, written) in read.iter().zip(&positions) {
                // To the sixth decimal place, give or take what a float is off by.
                prop_assert!((read.lat() - written.lat()).abs() <= 5.000_001e-7);
                prop_assert!((read.lon() - written.lon()).abs() <= 5.000_001e-7);
            }
            // What was read is written as the same text, letter for letter.
            prop_assert_eq!(&Polyline::through(&read).unwrap(), &line);
            prop_assert_eq!(&Polyline::new(line.as_str()).unwrap(), &line);
        }

        /// Whatever a service sends is read without a panic: it is a line or it is not.
        #[test]
        fn any_text_is_a_line_or_is_refused(text in "\\PC{0,60}") {
            if let Ok(line) = Polyline::new(text) {
                prop_assert!(line.positions().len() >= 2);
            }
        }

        #[test]
        fn any_letters_of_a_line_are_a_line_or_are_refused(text in "[?-~]{0,60}") {
            if let Ok(line) = Polyline::new(text.as_str()) {
                let positions = line.positions();
                prop_assert!(positions.len() >= 2);
                // What it reads as is on the globe, which a position always is.
                prop_assert!(Polyline::through(&positions).is_some());
            }
        }
    }

    #[test]
    fn what_a_distance_is_measured_along_has_one_text_everywhere() {
        crate::text::testing::assert_text_round_trips(Distances::ALL);
        assert_eq!(Distances::ALL, [Distances::GreatCircle, Distances::Road]);
        assert_eq!(Distances::Road.to_string(), "road");
    }
}
