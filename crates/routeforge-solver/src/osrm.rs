//! Road distances from an OSRM server (D66), and the way a route takes along its
//! roads (D73).
//!
//! The server's table service gives the distance by road from each of a list of
//! positions to each other. Its route service gives the way through a list of
//! positions in the order given, as a line. Both are asked with a `POST`, so that the
//! positions of a route of some hundred stops travel in a body, and not in an address
//! of several kilobytes that a server may refuse for its length.
//!
//! A road has a direction, so the way from A to B may be longer than the way back.
//! The table that comes of this says so, and what the solver makes of it is in
//! `route_job`. Whatever goes wrong here is an error and no more: the route job is the
//! one that falls back on great circles, and that stores a route without its way.

use std::time::Duration;

use anyhow::Context;
use reqwest::{Client, StatusCode, Url};
use routeforge_domain::{DistanceProvider, DistanceTable, Distances, LatLon, Polyline};
use serde::{Deserialize, Serialize};

use crate::config::Roads;

/// The profile the server is asked for: what travels on the roads. A server is
/// prepared for one profile, and `scripts/osrm-prepare` prepares it for cars.
const PROFILE: &str = "driving";

/// How long the server is given to take a connection. It is on this machine or on the
/// compose network, where a connection is made at once or not at all.
const CONNECT_WITHIN: Duration = Duration::from_secs(5);

/// Where a solver takes its distances from: the OSRM server it was given, and great
/// circles if it was given none (D8).
#[derive(Clone, Debug)]
pub(crate) enum Provider {
    GreatCircles,
    Osrm(Osrm),
}

impl Provider {
    /// Where a solver takes its distances from, by its settings: the routing service
    /// they name, and great circles if they name none.
    pub(crate) fn of(roads: Option<&Roads>) -> anyhow::Result<Self> {
        Ok(match roads {
            Some(roads) => Self::Osrm(Osrm::new(&roads.url, roads.within)?),
            None => Self::GreatCircles,
        })
    }
}

impl DistanceProvider for Provider {
    type Error = OsrmError;

    fn measures(&self) -> Distances {
        match self {
            Self::GreatCircles => Distances::GreatCircle,
            Self::Osrm(osrm) => osrm.measures(),
        }
    }

    async fn table(&self, positions: &[LatLon]) -> Result<DistanceTable, OsrmError> {
        match self {
            Self::GreatCircles => Ok(DistanceTable::haversine(positions)),
            Self::Osrm(osrm) => osrm.table(positions).await,
        }
    }

    async fn path(&self, positions: &[LatLon]) -> Result<Option<Polyline>, OsrmError> {
        match self {
            // The straight line from stop to stop is the way, and nobody is asked.
            Self::GreatCircles => Ok(None),
            Self::Osrm(osrm) => osrm.path(positions).await,
        }
    }
}

/// An OSRM server, as far as the solver asks it.
#[derive(Clone, Debug)]
pub(crate) struct Osrm {
    client: Client,
    /// Where its table service is.
    table: Url,
    /// Where its route service is.
    route: Url,
}

/// Why an OSRM server gave no table, or no way.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OsrmError {
    /// It did not answer, or not in time.
    #[error("the routing service does not answer: {0}")]
    Away(String),
    /// It answered that it will not: `code` is its own word for why, as `TooBig` for
    /// more positions than it was started to take.
    #[error("the routing service refused with `{code}`: {message}")]
    Refused { code: String, message: String },
    /// It answered what is no table of these positions, or no way through them.
    #[error("the routing service answered what cannot be read: {0}")]
    Unreadable(String),
    /// It knows no way from one position to another: one of them is cut off from the
    /// roads it has.
    #[error("the routing service knows no way from position {from} to position {to}")]
    NoWay { from: usize, to: usize },
}

/// What the table service is asked. The names are OSRM's.
#[derive(Serialize)]
struct Asked<'a> {
    /// Longitude first: OSRM's order, and not the one a position is said in.
    coordinates: &'a [[f64; 2]],
    /// Metres, where the service gives seconds unless it is told. In a body this is a
    /// list, where in an address it is a word: the service refuses the word here.
    annotations: [&'static str; 1],
    /// The positions as the service snapped them to its roads. Nothing here uses them.
    skip_waypoints: bool,
}

/// What the route service is asked. The names are OSRM's.
#[derive(Serialize)]
struct WayAsked<'a> {
    /// Longitude first, as for the table.
    coordinates: &'a [[f64; 2]],
    /// `full`: every point of the way. The service's own choice is the few points a
    /// picture of the whole route needs, and the line is to follow its streets when
    /// the map is looked at close up.
    overview: &'static str,
    /// `polyline6`: the way as the text a [`Polyline`] is.
    geometries: &'static str,
    /// Not the turns one by one.
    steps: bool,
    /// Whether the way has to go on from a stop in the direction it came to it. The
    /// table measures every leg by itself, whichever way the courier faces at its
    /// ends. For cars the service makes the way go straight on unless it is told
    /// otherwise, and that way is another one: against the server of D67, 153 legs
    /// of 331 came out longer than the table has them, the longest by two kilometres.
    /// With `false` the way is the one that was measured, to the metre.
    continue_straight: bool,
    /// As for the table.
    skip_waypoints: bool,
}

/// What a service answers, as far as it is read. With `code` anything but `Ok`,
/// `message` says why, and there is neither a table nor a route.
#[derive(Deserialize)]
struct Answered {
    code: String,
    #[serde(default)]
    message: Option<String>,
    /// Of the table service: row by row, in metres. `null` where there is no way.
    #[serde(default)]
    distances: Option<Vec<Vec<Option<f64>>>>,
    /// Of the route service: the routes it found, which is one unless it is asked
    /// for others beside it.
    #[serde(default)]
    routes: Option<Vec<Way>>,
}

/// A route of the route service, as far as it is read.
#[derive(Deserialize)]
struct Way {
    /// The way as a line, in the format that was asked for.
    geometry: String,
}

impl Osrm {
    /// The server at `base`, given `within` to answer one request.
    pub(crate) fn new(base: &Url, within: Duration) -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(CONNECT_WITHIN.min(within))
            .timeout(within)
            .build()
            .context("could not make the client the routing service is asked with")?;
        let service = |name: &str| {
            let base = base.as_str().trim_end_matches('/');
            Url::parse(&format!("{base}/{name}/v1/{PROFILE}"))
                .context("the address of the routing service")
        };
        Ok(Self {
            client,
            table: service("table")?,
            route: service("route")?,
        })
    }

    /// Asks one of the server's services, and reads its answer if it is one.
    async fn ask(&self, service: &Url, asked: &impl Serialize) -> Result<Answered, OsrmError> {
        let response = self
            .client
            .post(service.clone())
            .json(asked)
            .send()
            .await
            .map_err(|error| OsrmError::Away(causes(&error)))?;
        let status = response.status();
        let answered: Answered = response.json().await.map_err(|error| {
            if error.is_decode() {
                OsrmError::Unreadable(format!("{status}, and {}", causes(&error)))
            } else {
                // The answer began and did not end.
                OsrmError::Away(causes(&error))
            }
        })?;
        if status != StatusCode::OK || answered.code != "Ok" {
            return Err(OsrmError::Refused {
                code: answered.code,
                message: answered.message.unwrap_or_default(),
            });
        }
        Ok(answered)
    }
}

/// Positions as OSRM takes them: longitude first.
fn lon_lat(positions: &[LatLon]) -> Vec<[f64; 2]> {
    positions
        .iter()
        .map(|position| [position.lon(), position.lat()])
        .collect()
}

impl DistanceProvider for Osrm {
    type Error = OsrmError;

    fn measures(&self) -> Distances {
        Distances::Road
    }

    async fn table(&self, positions: &[LatLon]) -> Result<DistanceTable, OsrmError> {
        let nodes = positions.len();
        // The service takes two positions at least, and there is nothing to ask of it
        // about fewer: a place is no way from itself.
        if nodes < 2 {
            return DistanceTable::new(nodes, vec![0; nodes * nodes])
                .ok_or_else(|| OsrmError::Unreadable("no table of no positions".to_owned()));
        }
        let coordinates = lon_lat(positions);
        let asked = Asked {
            coordinates: &coordinates,
            annotations: ["distance"],
            skip_waypoints: true,
        };
        let answered = self.ask(&self.table, &asked).await?;
        let distances = answered
            .distances
            .ok_or_else(|| OsrmError::Unreadable("it has no `distances`".to_owned()))?;
        metres(nodes, &distances)
    }

    async fn path(&self, positions: &[LatLon]) -> Result<Option<Polyline>, OsrmError> {
        // The service takes two positions at least, and a place is no way from itself.
        if positions.len() < 2 {
            return Ok(None);
        }
        let coordinates = lon_lat(positions);
        let asked = WayAsked {
            coordinates: &coordinates,
            overview: "full",
            geometries: "polyline6",
            steps: false,
            continue_straight: false,
            skip_waypoints: true,
        };
        let answered = self.ask(&self.route, &asked).await?;
        let way = answered
            .routes
            .and_then(|routes| routes.into_iter().next())
            .ok_or_else(|| OsrmError::Unreadable("it has no route".to_owned()))?;
        let line = Polyline::new(way.geometry)
            .map_err(|error| OsrmError::Unreadable(format!("a way that is no line: {error}")))?;
        Ok(Some(line))
    }
}

/// The table OSRM gave, in whole metres (D4).
fn metres(nodes: usize, distances: &[Vec<Option<f64>>]) -> Result<DistanceTable, OsrmError> {
    if distances.len() != nodes || distances.iter().any(|row| row.len() != nodes) {
        return Err(OsrmError::Unreadable(format!(
            "a table of {} rows for {nodes} positions",
            distances.len()
        )));
    }
    let mut metres = Vec::with_capacity(nodes * nodes);
    for (from, row) in distances.iter().enumerate() {
        for (to, &distance) in row.iter().enumerate() {
            let distance = distance.ok_or(OsrmError::NoWay { from, to })?;
            // A NaN is in no range, so it is refused here as well.
            if !(0.0..=f64::from(u32::MAX)).contains(&distance) {
                return Err(OsrmError::Unreadable(format!(
                    "{distance} metres from position {from} to position {to}"
                )));
            }
            // A place is no way from itself, whatever the service makes of two
            // positions it has snapped to different sides of a street.
            metres.push(if from == to {
                0
            } else {
                distance.round() as u32
            });
        }
    }
    DistanceTable::new(nodes, metres)
        .ok_or_else(|| OsrmError::Unreadable("a table that is not square".to_owned()))
}

/// An error with what caused it, which is where reqwest says that a connection was
/// refused or that an answer took too long.
fn causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(deeper) = cause {
        text.push_str(&format!(": {deeper}"));
        cause = deeper.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn at(lat: f64, lon: f64) -> LatLon {
        LatLon::new(lat, lon).unwrap()
    }

    /// The depot of the demonstration and two places in Kyiv.
    fn three_places() -> [LatLon; 3] {
        [at(50.45, 30.52), at(50.40, 30.60), at(50.50, 30.45)]
    }

    fn osrm(server: &MockServer) -> Osrm {
        let base = Url::parse(&server.uri()).unwrap();
        Osrm::new(&base, Duration::from_secs(5)).unwrap()
    }

    /// A server whose `service`, `table` or `route`, answers every request with
    /// `answer`.
    async fn answering(service: &str, status: u16, answer: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/{service}/v1/driving")))
            .respond_with(ResponseTemplate::new(status).set_body_json(answer))
            .mount(&server)
            .await;
        server
    }

    /// The way through the three places, as the route service writes one.
    fn a_way() -> Polyline {
        Polyline::through(&three_places()).unwrap()
    }

    /// What the route service answers with a way, with what it says beside it.
    fn a_route_along(line: &str) -> Value {
        json!({
            "code": "Ok",
            "routes": [{
                "geometry": line,
                "distance": 43_421,
                "duration": 3_014.3,
                "weight": 3_014.3,
                "weight_name": "routability",
                "legs": [{ "distance": 13_757, "duration": 1_002.1, "steps": [], "summary": "" }],
            }],
        })
    }

    #[tokio::test]
    async fn the_table_is_asked_for_in_one_post_with_the_longitude_first() {
        let server = MockServer::start().await;
        let asked = json!({
            // OSRM says a position as longitude, latitude.
            "coordinates": [[30.52, 50.45], [30.60, 50.40], [30.45, 50.50]],
            // A list in a body. OSRM v26.10 answers the bare word with `InvalidQuery`.
            "annotations": ["distance"],
            "skip_waypoints": true,
        });
        let answer = json!({
            "code": "Ok",
            "distances": [[0, 9_876.4, 8_120.5], [10_321.6, 0, 17_400.49], [8_005, 16_999.5, 0]],
        });
        Mock::given(method("POST"))
            .and(path("/table/v1/driving"))
            .and(header("content-type", "application/json"))
            .and(body_json(asked))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .expect(1)
            .mount(&server)
            .await;

        let roads = osrm(&server);
        assert_eq!(roads.measures(), Distances::Road);
        let table = roads.table(&three_places()).await.unwrap();

        // Whole metres, and the way there kept apart from the way back.
        let expected = [[0, 9_876, 8_121], [10_322, 0, 17_400], [8_005, 17_000, 0]];
        assert_eq!(table.nodes(), 3);
        for (from, row) in expected.iter().enumerate() {
            for (to, &metres) in row.iter().enumerate() {
                assert_eq!(table.get(from, to), metres, "{from} to {to}");
            }
        }
    }

    /// A base with a path of its own, as behind a proxy, and a slash at its end.
    #[tokio::test]
    async fn the_service_may_be_under_a_path() {
        let server = MockServer::start().await;
        let answer = json!({ "code": "Ok", "distances": [[0, 5], [7, 0]] });
        Mock::given(method("POST"))
            .and(path("/roads/table/v1/driving"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/roads/", server.uri())).unwrap();
        let roads = Osrm::new(&base, Duration::from_secs(5)).unwrap();

        let table = roads.table(&three_places()[..2]).await.unwrap();
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
    }

    #[tokio::test]
    async fn fewer_than_two_positions_are_not_asked_about() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let roads = osrm(&server);

        let alone = roads.table(&three_places()[..1]).await.unwrap();
        assert_eq!((alone.nodes(), alone.get(0, 0)), (1, 0));
        assert_eq!(roads.table(&[]).await.unwrap().nodes(), 0);
    }

    #[tokio::test]
    async fn the_way_from_a_place_to_itself_is_no_way() {
        // Two positions on either side of a street can be snapped apart.
        let answer = json!({ "code": "Ok", "distances": [[3.4, 5], [7, 12]] });
        let server = answering("table", 200, answer).await;
        let table = osrm(&server).table(&three_places()[..2]).await.unwrap();
        assert_eq!((table.get(0, 0), table.get(1, 1)), (0, 0));
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
    }

    #[tokio::test]
    async fn a_refusal_is_passed_on_in_the_words_of_the_service() {
        let too_big = json!({ "code": "TooBig", "message": "Too many table coordinates" });
        let server = answering("table", 400, too_big).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert_eq!(
            error,
            OsrmError::Refused {
                code: "TooBig".to_owned(),
                message: "Too many table coordinates".to_owned(),
            }
        );
        assert!(error.to_string().contains("TooBig"), "{error}");

        // A refusal that comes with a 200 is a refusal all the same.
        let no_table = json!({ "code": "NoTable", "message": "No table found" });
        let server = answering("table", 200, no_table).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert!(
            matches!(&error, OsrmError::Refused { code, .. } if code == "NoTable"),
            "{error:?}"
        );
        // And so is an `Ok` that comes with anything but a 200.
        let odd = json!({ "code": "Ok", "distances": [[0, 1, 2], [1, 0, 2], [2, 1, 0]] });
        let server = answering("table", 503, odd).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert!(matches!(error, OsrmError::Refused { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_place_that_is_cut_off_is_no_table() {
        let answer = json!({
            "code": "Ok",
            "distances": [[0, 100, null], [100, 0, null], [null, null, 0]],
        });
        let server = answering("table", 200, answer).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert_eq!(error, OsrmError::NoWay { from: 0, to: 2 });
    }

    #[tokio::test]
    async fn what_is_no_table_of_these_positions_is_refused() {
        for (answer, what) in [
            (json!({ "code": "Ok" }), "no table at all"),
            (
                json!({ "code": "Ok", "distances": [[0, 1], [1, 0]] }),
                "too few rows",
            ),
            (
                json!({ "code": "Ok", "distances": [[0, 1, 2], [1, 0], [2, 1, 0]] }),
                "a short row",
            ),
            (
                // Nine distances, as three positions have, and no three rows of three.
                json!({ "code": "Ok", "distances": [[0, 1], [1, 0, 2, 2], [2, 1, 0]] }),
                "a short row made up for by a long one",
            ),
            (
                json!({ "code": "Ok", "distances": [[0, -1, 2], [1, 0, 2], [2, 1, 0]] }),
                "a distance below nothing",
            ),
            (
                json!({ "code": "Ok", "distances": [[0, 1e12, 2], [1, 0, 2], [2, 1, 0]] }),
                "a distance no route has",
            ),
            (
                json!({ "distances": [[0, 1, 2], [1, 0, 2], [2, 1, 0]] }),
                "no code",
            ),
            (json!("a table"), "no object"),
        ] {
            let server = answering("table", 200, answer).await;
            let error = osrm(&server).table(&three_places()).await.unwrap_err();
            assert!(
                matches!(error, OsrmError::Unreadable(_)),
                "{what}: {error:?}"
            );
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>a proxy</html>"))
            .mount(&server)
            .await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert!(matches!(error, OsrmError::Unreadable(_)), "{error:?}");
        // What it was answered with says where to look.
        assert!(error.to_string().contains("200"), "{error}");
    }

    #[tokio::test]
    async fn a_service_that_takes_too_long_is_away() {
        let server = MockServer::start().await;
        let late = ResponseTemplate::new(200)
            .set_body_json(json!({ "code": "Ok", "distances": [[0, 5], [7, 0]] }))
            .set_delay(Duration::from_secs(30));
        Mock::given(method("POST"))
            .respond_with(late)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let roads = Osrm::new(&base, Duration::from_millis(200)).unwrap();

        let started = std::time::Instant::now();
        let error = roads.table(&three_places()[..2]).await.unwrap_err();
        assert!(matches!(error, OsrmError::Away(_)), "{error:?}");
        // And no sooner with a way than with a table.
        let error = roads.path(&three_places()).await.unwrap_err();
        assert!(matches!(error, OsrmError::Away(_)), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn the_way_is_asked_for_in_one_post_with_the_longitude_first() {
        let server = MockServer::start().await;
        let asked = json!({
            // In the order they are to be passed, each as longitude, latitude.
            "coordinates": [[30.52, 50.45], [30.60, 50.40], [30.45, 50.50]],
            // Every point of the way, as text with six decimal places.
            "overview": "full",
            "geometries": "polyline6",
            "steps": false,
            // The way the table measured. OSRM v26.10 takes a boolean here, and
            // answers the word with `InvalidQuery`.
            "continue_straight": false,
            "skip_waypoints": true,
        });
        Mock::given(method("POST"))
            .and(path("/route/v1/driving"))
            .and(header("content-type", "application/json"))
            .and(body_json(asked))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_route_along(a_way().as_str())))
            .expect(1)
            .mount(&server)
            .await;

        let way = osrm(&server).path(&three_places()).await.unwrap();
        assert_eq!(way, Some(a_way()));
    }

    #[tokio::test]
    async fn the_way_is_asked_for_under_the_path_the_service_is_under() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/roads/route/v1/driving"))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_route_along(a_way().as_str())))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/roads/", server.uri())).unwrap();
        let roads = Osrm::new(&base, Duration::from_secs(5)).unwrap();

        assert_eq!(roads.path(&three_places()).await.unwrap(), Some(a_way()));
    }

    #[tokio::test]
    async fn the_way_through_fewer_than_two_positions_is_not_asked_for() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let roads = osrm(&server);

        assert_eq!(roads.path(&three_places()[..1]).await.unwrap(), None);
        assert_eq!(roads.path(&[]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_way_that_is_refused_is_refused_in_the_words_of_the_service() {
        let too_big = json!({
            "code": "TooBig",
            "message": "Number of entries 501 is higher than current maximum (500)",
        });
        let server = answering("route", 400, too_big).await;
        let error = osrm(&server).path(&three_places()).await.unwrap_err();
        assert!(
            matches!(&error, OsrmError::Refused { code, message }
                if code == "TooBig" && message.contains("501")),
            "{error:?}"
        );

        let no_route = json!({ "code": "NoRoute", "message": "Impossible route between points" });
        let server = answering("route", 400, no_route).await;
        let error = osrm(&server).path(&three_places()).await.unwrap_err();
        assert!(
            matches!(&error, OsrmError::Refused { code, .. } if code == "NoRoute"),
            "{error:?}"
        );
        // An `Ok` that comes with anything but a 200 is a refusal too.
        let server = answering("route", 503, a_route_along(a_way().as_str())).await;
        let error = osrm(&server).path(&three_places()).await.unwrap_err();
        assert!(matches!(error, OsrmError::Refused { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn what_is_no_way_through_these_positions_is_refused() {
        let depot = three_places()[0];
        let there_twice = Polyline::through(&[depot, depot]).unwrap();
        // The place once: the last two letters say that the second point is no
        // further north and no further east.
        let one_point = &there_twice.as_str()[..there_twice.as_str().len() - 2];
        for (answer, what) in [
            (json!({ "code": "Ok" }), "no routes at all"),
            (json!({ "code": "Ok", "routes": [] }), "no route among them"),
            (
                json!({ "code": "Ok", "routes": [{ "distance": 43_421 }] }),
                "a route without its way",
            ),
            (a_route_along("not a line"), "a way that is no line"),
            (a_route_along(""), "a way of no points"),
            (a_route_along(one_point), "a way of one point"),
            (
                // What the service sends when it is asked for `geojson`.
                json!({ "code": "Ok", "routes": [{ "geometry": {
                    "type": "LineString",
                    "coordinates": [[30.52, 50.45], [30.60, 50.40]],
                } }] }),
                "a way in another format",
            ),
            (
                // A table, which is what the other service answers.
                json!({ "code": "Ok", "distances": [[0, 1, 2], [1, 0, 2], [2, 1, 0]] }),
                "a table",
            ),
        ] {
            let server = answering("route", 200, answer).await;
            let error = osrm(&server).path(&three_places()).await.unwrap_err();
            assert!(
                matches!(error, OsrmError::Unreadable(_)),
                "{what}: {error:?}"
            );
        }
    }

    /// Of several routes the first is the one the service thinks best, and the one
    /// that was asked for.
    #[tokio::test]
    async fn the_way_is_the_first_route_of_the_answer() {
        let other = Polyline::through(&three_places()[..2]).unwrap();
        let mut answer = a_route_along(a_way().as_str());
        let alternative = a_route_along(other.as_str())["routes"][0].clone();
        answer["routes"].as_array_mut().unwrap().push(alternative);
        let server = answering("route", 200, answer).await;

        assert_eq!(
            osrm(&server).path(&three_places()).await.unwrap(),
            Some(a_way())
        );
    }

    /// What a solver is started with decides whom it asks: the service its settings
    /// name, and nobody if they name none (D8).
    #[tokio::test]
    async fn a_solver_asks_the_service_it_was_given_and_nobody_without_one() {
        let places = three_places();
        let provider = Provider::of(None).unwrap();
        assert_eq!(provider.measures(), Distances::GreatCircle);
        assert_eq!(
            provider.table(&places).await.unwrap(),
            DistanceTable::haversine(&places)
        );
        // And the way between great circles is the straight line, which is no answer.
        assert_eq!(provider.path(&places).await.unwrap(), None);

        let answer = json!({ "code": "Ok", "distances": [[0, 5], [7, 0]] });
        let server = answering("table", 200, answer).await;
        Mock::given(method("POST"))
            .and(path("/route/v1/driving"))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_route_along(a_way().as_str())))
            .mount(&server)
            .await;
        let roads = Roads {
            url: Url::parse(&server.uri()).unwrap(),
            within: Duration::from_secs(5),
        };
        let provider = Provider::of(Some(&roads)).unwrap();
        assert_eq!(provider.measures(), Distances::Road);
        let table = provider.table(&places[..2]).await.unwrap();
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(provider.path(&places).await.unwrap(), Some(a_way()));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
