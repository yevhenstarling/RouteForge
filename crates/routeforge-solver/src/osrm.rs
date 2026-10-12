//! Road distances from an OSRM server (D66).
//!
//! The server's table service gives the distance by road from each of a list of
//! positions to each other. It is asked with a `POST`, so that the positions of a
//! route of some hundred stops travel in a body, and not in an address of several
//! kilobytes that a server may refuse for its length.
//!
//! A road has a direction, so the way from A to B may be longer than the way back.
//! The table that comes of this says so, and what the solver makes of it is in
//! `route_job`. Whatever goes wrong here is an error and no more: the route job is the
//! one that falls back on great circles.

use std::time::Duration;

use anyhow::Context;
use reqwest::{Client, StatusCode, Url};
use routeforge_domain::{DistanceProvider, DistanceTable, Distances, LatLon};
use serde::{Deserialize, Serialize};

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
}

/// An OSRM server, as far as the solver asks it.
#[derive(Clone, Debug)]
pub(crate) struct Osrm {
    client: Client,
    /// Where its table service is.
    table: Url,
}

/// Why an OSRM server gave no table.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OsrmError {
    /// It did not answer, or not in time.
    #[error("the routing service does not answer: {0}")]
    Away(String),
    /// It answered that it will not: `code` is its own word for why, as `TooBig` for
    /// more positions than it was started to take.
    #[error("the routing service refused with `{code}`: {message}")]
    Refused { code: String, message: String },
    /// It answered what is no table of these positions.
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

/// What the table service answers. With `code` anything but `Ok`, `message` says why
/// and there is no table.
#[derive(Deserialize)]
struct Answered {
    code: String,
    #[serde(default)]
    message: Option<String>,
    /// Row by row, in metres. `null` where there is no way.
    #[serde(default)]
    distances: Option<Vec<Vec<Option<f64>>>>,
}

impl Osrm {
    /// The server at `base`, given `within` to answer a request for a table.
    pub(crate) fn new(base: &Url, within: Duration) -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(CONNECT_WITHIN.min(within))
            .timeout(within)
            .build()
            .context("could not make the client the routing service is asked with")?;
        let table = format!("{}/table/v1/{PROFILE}", base.as_str().trim_end_matches('/'));
        let table = Url::parse(&table).context("the address of the routing service")?;
        Ok(Self { client, table })
    }
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
        let coordinates: Vec<[f64; 2]> = positions
            .iter()
            .map(|position| [position.lon(), position.lat()])
            .collect();
        let asked = Asked {
            coordinates: &coordinates,
            annotations: ["distance"],
            skip_waypoints: true,
        };
        let response = self
            .client
            .post(self.table.clone())
            .json(&asked)
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
        let distances = answered
            .distances
            .ok_or_else(|| OsrmError::Unreadable("it has no `distances`".to_owned()))?;
        metres(nodes, &distances)
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

    /// A server that answers every request for a table with `answer`.
    async fn answering(status: u16, answer: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/table/v1/driving"))
            .respond_with(ResponseTemplate::new(status).set_body_json(answer))
            .mount(&server)
            .await;
        server
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
        let server = answering(200, answer).await;
        let table = osrm(&server).table(&three_places()[..2]).await.unwrap();
        assert_eq!((table.get(0, 0), table.get(1, 1)), (0, 0));
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
    }

    #[tokio::test]
    async fn a_refusal_is_passed_on_in_the_words_of_the_service() {
        let too_big = json!({ "code": "TooBig", "message": "Too many table coordinates" });
        let server = answering(400, too_big).await;
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
        let server = answering(200, no_table).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert!(
            matches!(&error, OsrmError::Refused { code, .. } if code == "NoTable"),
            "{error:?}"
        );
        // And so is an `Ok` that comes with anything but a 200.
        let odd = json!({ "code": "Ok", "distances": [[0, 1, 2], [1, 0, 2], [2, 1, 0]] });
        let server = answering(503, odd).await;
        let error = osrm(&server).table(&three_places()).await.unwrap_err();
        assert!(matches!(error, OsrmError::Refused { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_place_that_is_cut_off_is_no_table() {
        let answer = json!({
            "code": "Ok",
            "distances": [[0, 100, null], [100, 0, null], [null, null, 0]],
        });
        let server = answering(200, answer).await;
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
            let server = answering(200, answer).await;
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
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn without_a_service_the_distances_are_great_circles() {
        let places = three_places();
        let provider = Provider::GreatCircles;
        assert_eq!(provider.measures(), Distances::GreatCircle);
        assert_eq!(
            provider.table(&places).await.unwrap(),
            DistanceTable::haversine(&places)
        );

        let answer = json!({ "code": "Ok", "distances": [[0, 5], [7, 0]] });
        let server = answering(200, answer).await;
        let provider = Provider::Osrm(osrm(&server));
        assert_eq!(provider.measures(), Distances::Road);
        let table = provider.table(&places[..2]).await.unwrap();
        assert_eq!((table.get(0, 1), table.get(1, 0)), (5, 7));
    }
}
