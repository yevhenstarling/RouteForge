//! Where an address is: asked of a geocoding service (D72).
//!
//! A person who cannot share a location can write an address, and the bot asks a
//! service where that is. The service is one that speaks as Nominatim does, which is
//! the search of OpenStreetMap. There is none unless the bot is given one: on a clone
//! the bot asks nobody but Telegram and the API, and takes a place as a location or as
//! coordinates.
//!
//! Whoever points the bot at the public Nominatim takes on its usage policy,
//! <https://operations.osmfoundation.org/policies/nominatim/>. What the bot does for it
//! is here: it says who it is, it sends one request a second at the most, whoever
//! asks, and it asks only when a person has written an address. Showing where the
//! places are from is the dialogue's part.
//!
//! [`Geocoder`] is what the dialogue is written against, so that a test can put
//! something else in its place.

use std::error::Error;
use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use reqwest::{Client, StatusCode, Url};
use routeforge_domain::LatLon;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// How many places a search gives at the most: as many as there are buttons for.
pub(crate) const MOST_FOUND: usize = 3;

/// How many places the service is asked for: more than are shown. One address is
/// often several things on its map, each building of it and each shop in them, and
/// several of those come back under one name. What is left of ten is what a person
/// can tell apart.
const ASKED_FOR: usize = 10;

/// How the bot says who it is: its name and version, and where it comes from. A
/// service that is asked by many wants to know by whom, and Nominatim's policy asks
/// for it.
const USER_AGENT: &str = concat!(
    "routeforge-bot/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// How long the service is given to take a connection, and to answer. The person is
/// waiting in a chat.
const CONNECT_WITHIN: Duration = Duration::from_secs(5);
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// How far apart two requests are at the least: the public Nominatim takes one a
/// second from an application, all its users together.
const APART: Duration = Duration::from_secs(1);

/// How long a search waits for its turn before the person is told to try again.
const TURN_WITHIN: Duration = Duration::from_secs(5);

/// How far from the place it is near a search prefers its answers, in degrees north and
/// south of it: some 55 kilometres, which is a city and what lies round it.
const NEAR_DEGREES: f64 = 0.5;

/// A place that a search for an address found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Found {
    pub(crate) location: LatLon,
    /// What the service calls the place: a street and a number, a town, a country.
    pub(crate) name: String,
}

/// Why an address could not be looked up.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Unfound {
    /// The service did not answer, or answered what the bot cannot read.
    #[error("the address service does not answer: {0}")]
    Away(String),
    /// More searches are waiting for their turn than the bot lets wait.
    #[error("too many addresses are being looked up at this moment")]
    Busy,
}

/// Something that finds where an address is.
pub(crate) trait Geocoder: Send + Sync + 'static {
    /// Whether there is anything to ask. Without it the bot takes a place as a
    /// location or as coordinates, and words as it always did.
    fn is_on(&self) -> bool;

    /// The places `text` may mean, the likeliest first: [`MOST_FOUND`] at the most, and
    /// no two under one name. None at all is an answer, and no error. Places `near`
    /// the given one are preferred: the depot, if the bot knows where it is.
    fn find(
        &self,
        text: &str,
        near: Option<LatLon>,
    ) -> impl Future<Output = Result<Vec<Found>, Unfound>> + Send;
}

/// A bot that was given no service has none, and one that was has that one.
impl<G: Geocoder> Geocoder for Option<G> {
    fn is_on(&self) -> bool {
        self.as_ref().is_some_and(Geocoder::is_on)
    }

    async fn find(&self, text: &str, near: Option<LatLon>) -> Result<Vec<Found>, Unfound> {
        match self {
            Some(geocoder) => geocoder.find(text, near).await,
            None => Ok(Vec::new()),
        }
    }
}

/// A service that speaks as Nominatim does, over HTTP.
pub(crate) struct Nominatim {
    client: Client,
    /// Where its search is.
    search: Url,
    /// When the next request may be sent. Whoever holds this decides when theirs is.
    next: Mutex<Instant>,
    apart: Duration,
    turn_within: Duration,
}

/// One place of an answer, as the service writes it: the two numbers as text.
#[derive(Deserialize)]
struct Hit {
    lat: String,
    lon: String,
    display_name: String,
}

impl Nominatim {
    /// The service at `base`.
    pub(crate) fn new(base: &Url) -> anyhow::Result<Self> {
        Self::paced(base, APART, TURN_WITHIN, ANSWER_WITHIN)
    }

    /// The service at `base`, asked at most once in `apart`, by a search that waits
    /// `turn_within` for its turn and `answer_within` for the answer.
    pub(crate) fn paced(
        base: &Url,
        apart: Duration,
        turn_within: Duration,
        answer_within: Duration,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_WITHIN.min(answer_within))
            .timeout(answer_within)
            .build()
            .context("could not make the client the address service is asked with")?;
        let search = format!("{}/search", base.as_str().trim_end_matches('/'));
        let search = Url::parse(&search).context("the address of the address service")?;
        Ok(Self {
            client,
            search,
            next: Mutex::new(Instant::now()),
            apart,
            turn_within,
        })
    }

    /// Waits until this search may send its request: `apart` after the one before it.
    ///
    /// # Errors
    ///
    /// [`Unfound::Busy`], at once, if that is further off than a search waits. Nothing
    /// was queued then, so the ones that do wait are not kept waiting longer by it.
    async fn turn(&self) -> Result<(), Unfound> {
        let at = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let at = (*next).max(now);
            if at.duration_since(now) > self.turn_within {
                return Err(Unfound::Busy);
            }
            *next = at + self.apart;
            at
        };
        tokio::time::sleep_until(at).await;
        Ok(())
    }
}

impl Geocoder for Nominatim {
    fn is_on(&self) -> bool {
        true
    }

    async fn find(&self, text: &str, near: Option<LatLon>) -> Result<Vec<Found>, Unfound> {
        self.turn().await?;
        let mut asked = self.client.get(self.search.clone()).query(&[
            ("q", text),
            ("format", "jsonv2"),
            ("limit", &ASKED_FOR.to_string()),
        ]);
        if let Some(near) = near {
            asked = asked.query(&[("viewbox", view_round(near))]);
        }
        let response = asked
            .send()
            .await
            .map_err(|error| Unfound::Away(causes(error)))?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(Unfound::Away(format!("it answered {status}")));
        }
        let hits: Vec<Hit> = response.json().await.map_err(|error| {
            Unfound::Away(format!(
                "it answered what cannot be read: {}",
                causes(error)
            ))
        })?;
        Ok(told_apart(hits.into_iter().filter_map(found)))
    }
}

/// The first [`MOST_FOUND`] places that differ by name, in the order they came in.
///
/// Two places under one name are one place to whoever reads the list: there is
/// nothing to choose between them by. The first is the one the service thinks the
/// likeliest, and the others are some tens of metres from it.
fn told_apart(places: impl Iterator<Item = Found>) -> Vec<Found> {
    let mut apart: Vec<Found> = Vec::with_capacity(MOST_FOUND);
    for place in places {
        if apart.len() == MOST_FOUND {
            break;
        }
        if apart.iter().all(|known| known.name != place.name) {
            apart.push(place);
        }
    }
    apart
}

/// A place of an answer, if it is one on the globe with a name to show.
fn found(hit: Hit) -> Option<Found> {
    let location = LatLon::new(hit.lat.trim().parse().ok()?, hit.lon.trim().parse().ok()?).ok()?;
    let name = hit.display_name.trim();
    (!name.is_empty()).then(|| Found {
        location,
        name: name.to_owned(),
    })
}

/// The box round a place that a search is to prefer, as the service takes it: two
/// corners, each as a longitude and a latitude. A preference and no limit: what lies
/// outside it is found as well, further down.
fn view_round(near: LatLon) -> String {
    // A degree of longitude is shorter than one of latitude, by the cosine of the
    // latitude. Near a pole the box is the whole way round, which prefers nothing.
    let east = (NEAR_DEGREES / near.lat().to_radians().cos().abs().max(1e-6)).min(180.0);
    let (west, east) = (
        (near.lon() - east).max(-180.0),
        (near.lon() + east).min(180.0),
    );
    let (south, north) = (
        (near.lat() - NEAR_DEGREES).max(-90.0),
        (near.lat() + NEAR_DEGREES).min(90.0),
    );
    format!("{west:.4},{south:.4},{east:.4},{north:.4}")
}

/// An error with what caused it, which is where reqwest says that a connection was
/// refused or that an answer took too long.
///
/// Without the address that was asked: reqwest names it in its errors, and in this one
/// it ends in what a person wrote. That is for the service and for nobody's log.
fn causes(error: reqwest::Error) -> String {
    let error = error.without_url();
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const QUICK: Duration = Duration::from_millis(1);
    const LONG: Duration = Duration::from_secs(30);

    /// The service of a server, with next to no pause between two requests.
    fn service(server: &MockServer) -> Nominatim {
        let base = Url::parse(&server.uri()).unwrap();
        Nominatim::paced(&base, QUICK, LONG, LONG).unwrap()
    }

    async fn answering(status: u16, answer: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(status).set_body_json(answer))
            .mount(&server)
            .await;
        server
    }

    fn khreshchatyk() -> Value {
        json!({
            "place_id": 191_604_402,
            "licence": "Data © OpenStreetMap contributors, ODbL 1.0.",
            "osm_type": "way",
            "lat": "50.4476532",
            "lon": "30.5222416",
            "category": "building",
            "type": "yes",
            "importance": 0.00001,
            "display_name": "22, Хрещатик вулиця, Липки, Київ, 01001, Україна",
            "boundingbox": ["50.4474", "50.4478", "30.5218", "30.5226"],
        })
    }

    #[tokio::test]
    async fn an_address_is_asked_for_as_nominatim_takes_it_and_the_bot_says_who_it_is() {
        let server = answering(200, json!([khreshchatyk()])).await;
        let depot = LatLon::new(50.45, 30.52).unwrap();

        let found = service(&server)
            .find("Хрещатик 22", Some(depot))
            .await
            .unwrap();
        assert_eq!(
            found,
            [Found {
                location: LatLon::new(50.447_653_2, 30.522_241_6).unwrap(),
                name: "22, Хрещатик вулиця, Липки, Київ, 01001, Україна".to_owned(),
            }]
        );

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let asked: std::collections::BTreeMap<String, String> = requests[0]
            .url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        let expected = [
            ("format", "jsonv2"),
            // More than are shown: see `places_under_one_name_are_one_place`.
            ("limit", "10"),
            ("q", "Хрещатик 22"),
            // West, south, east, north: half a degree up and down, and as far in
            // kilometres to either side, which at this latitude is 0.785 degrees.
            ("viewbox", "29.7348,49.9500,31.3052,50.9500"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()));
        assert_eq!(asked, expected.into());

        // Who asks: the bot by its name and version, and where its source is.
        let agent = requests[0].headers.get("user-agent").unwrap();
        let agent = agent.to_str().unwrap();
        assert!(agent.starts_with("routeforge-bot/"), "{agent}");
        assert!(agent.contains("github.com"), "{agent}");
        // And nothing of whose address it is: no key, no cookie, nobody's token.
        for header in ["authorization", "cookie", "x-request-id"] {
            assert!(requests[0].headers.get(header).is_none(), "{header}");
        }
    }

    #[tokio::test]
    async fn without_a_place_to_be_near_nothing_is_preferred() {
        let server = answering(200, json!([khreshchatyk()])).await;
        service(&server).find("Хрещатик 22", None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let named: Vec<String> = requests[0]
            .url
            .query_pairs()
            .map(|(name, _)| name.into_owned())
            .collect();
        assert_eq!(named, ["q", "format", "limit"]);
    }

    #[tokio::test]
    async fn a_service_behind_a_path_is_asked_under_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nominatim/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let base = Url::parse(&format!("{}/nominatim/", server.uri())).unwrap();
        let service = Nominatim::paced(&base, QUICK, LONG, LONG).unwrap();
        assert_eq!(service.find("anywhere", None).await.unwrap(), []);
    }

    #[tokio::test]
    async fn nothing_found_is_an_answer_and_at_most_three_places_are_taken() {
        let nowhere = answering(200, json!([])).await;
        assert_eq!(
            service(&nowhere).find("qwertyuiop", None).await.unwrap(),
            []
        );

        let place = |number: u32| {
            json!({
                "lat": format!("50.{number}"),
                "lon": "30.5",
                "display_name": format!("Place {number}"),
            })
        };
        let many = answering(200, json!((1..=7).map(place).collect::<Vec<_>>())).await;
        let found = service(&many).find("a street", None).await.unwrap();
        let names: Vec<&str> = found.iter().map(|place| place.name.as_str()).collect();
        assert_eq!(names, ["Place 1", "Place 2", "Place 3"]);
    }

    /// What the public service answers for `Хрещатик 22`, in short: the address has
    /// three buildings, and a post office in one of them.
    #[tokio::test]
    async fn places_under_one_name_are_one_place() {
        const ADDRESS: &str = "22, вулиця Хрещатик, Центр, Київ, 01001, Україна";
        const POST: &str = "Укрпошта 01001, 22, вулиця Хрещатик, Центр, Київ, 01001, Україна";
        const ELSEWHERE: &str = "22, вулиця Хрещатик, Черкаси, 18001, Україна";
        let place =
            |lat: &str, name: &str| json!({ "lat": lat, "lon": "30.5231", "display_name": name });
        let answer = json!([
            place("50.44985", ADDRESS),
            place("50.45011", ADDRESS),
            place("50.44992", POST),
            place("50.44997", ADDRESS),
            place("49.44420", ELSEWHERE),
            place("49.44421", "A fourth place, which there is no button for"),
        ]);
        let server = answering(200, answer).await;

        let found = service(&server).find("Хрещатик 22", None).await.unwrap();
        let shown: Vec<(f64, &str)> = found
            .iter()
            .map(|place| (place.location.lat(), place.name.as_str()))
            .collect();
        // The first of a name is the one that stays: the likeliest, by the service.
        assert_eq!(
            shown,
            [(50.44985, ADDRESS), (50.44992, POST), (49.4442, ELSEWHERE)]
        );
    }

    #[tokio::test]
    async fn a_place_that_is_none_is_passed_over() {
        let answer = json!([
            { "lat": "95.0", "lon": "30.5", "display_name": "Off the globe" },
            { "lat": "north", "lon": "30.5", "display_name": "No number" },
            { "lat": "50.4", "lon": "30.5", "display_name": "  " },
            { "lat": " 50.4 ", "lon": "30.5", "display_name": " A place " },
        ]);
        let server = answering(200, answer).await;
        let found = service(&server).find("somewhere", None).await.unwrap();
        assert_eq!(
            found,
            [Found {
                location: LatLon::new(50.4, 30.5).unwrap(),
                name: "A place".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_service_that_refuses_or_cannot_be_read_is_away() {
        for (status, answer) in [
            (503, json!({ "error": "busy" })),
            (403, json!("blocked")),
            (200, json!({ "error": "not a list" })),
            (
                200,
                json!([{ "lat": 50.4, "lon": 30.5, "display_name": "numbers" }]),
            ),
        ] {
            let server = answering(status, answer).await;
            let error = service(&server).find("anywhere", None).await.unwrap_err();
            assert!(matches!(error, Unfound::Away(_)), "{status}: {error:?}");
        }

        let server = MockServer::start().await;
        let late = ResponseTemplate::new(200)
            .set_body_json(json!([]))
            .set_delay(Duration::from_secs(30));
        Mock::given(method("GET"))
            .respond_with(late)
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let impatient = Nominatim::paced(&base, QUICK, LONG, Duration::from_millis(200)).unwrap();
        let error = impatient.find("anywhere", None).await.unwrap_err();
        assert!(matches!(error, Unfound::Away(_)), "{error:?}");
    }

    /// What a person wrote goes to the service. It does not go into what is said of a
    /// failure, which is on its way to a log: reqwest names the address it asked in
    /// its errors, and here that address ends in the words.
    #[tokio::test]
    async fn what_was_asked_for_is_not_in_what_is_said_of_a_failure() {
        // Nobody listens where the service was said to be.
        let nobody = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let nowhere = Url::parse(&format!("http://{}", nobody.local_addr().unwrap())).unwrap();
        drop(nobody);
        let unreachable = Nominatim::paced(&nowhere, QUICK, LONG, LONG).unwrap();

        // It answers too late.
        let late = MockServer::start().await;
        let after = ResponseTemplate::new(200)
            .set_body_json(json!([]))
            .set_delay(Duration::from_secs(30));
        Mock::given(method("GET"))
            .respond_with(after)
            .mount(&late)
            .await;
        let base = Url::parse(&late.uri()).unwrap();
        let impatient = Nominatim::paced(&base, QUICK, LONG, Duration::from_millis(200)).unwrap();

        // It answers what is no list of places.
        let unreadable = answering(200, json!({ "error": "Хрещатик 22" })).await;

        let asked = "Хрещатик 22, де живе Ада";
        let depot = LatLon::new(50.45, 30.52).unwrap();
        for (service, says) in [
            (&unreachable, "error sending request"),
            (&impatient, "timed out"),
            (&service(&unreadable), "cannot be read"),
        ] {
            let error = service.find(asked, Some(depot)).await.unwrap_err();
            let said = format!("{error} {error:?}");
            assert!(said.contains(says), "{said}");
            // Not as it was written, not as it is written into an address, and not
            // the address it was asked at either.
            for part in ["Хрещатик", "Ада", "%D0", "q=", "search", "viewbox", "http"] {
                assert!(!said.contains(part), "{part} in: {said}");
            }
        }
    }

    /// The public service takes one request a second from all the users of an
    /// application together. So the searches of every chat take turns.
    #[tokio::test]
    async fn two_requests_are_never_nearer_each_other_than_they_may_be() {
        let server = answering(200, json!([])).await;
        let base = Url::parse(&server.uri()).unwrap();
        let apart = Duration::from_millis(300);
        let service = Nominatim::paced(&base, apart, LONG, LONG).unwrap();

        let began = std::time::Instant::now();
        let (first, second, third) = tokio::join!(
            service.find("one", None),
            service.find("two", None),
            service.find("three", None),
        );
        assert!(first.is_ok() && second.is_ok() && third.is_ok());
        assert!(began.elapsed() >= 2 * apart, "{:?}", began.elapsed());

        // After a pause, the next goes out at once.
        tokio::time::sleep(apart).await;
        let began = std::time::Instant::now();
        service.find("four", None).await.unwrap();
        assert!(began.elapsed() < apart, "{:?}", began.elapsed());
    }

    #[tokio::test]
    async fn a_search_that_would_wait_too_long_for_its_turn_is_told_so_at_once() {
        let server = answering(200, json!([])).await;
        let base = Url::parse(&server.uri()).unwrap();
        let service = Nominatim::paced(
            &base,
            Duration::from_secs(10),
            Duration::from_millis(100),
            LONG,
        )
        .unwrap();

        // The first goes out at once, and the next would be ten seconds after it.
        service.find("one", None).await.unwrap();
        let began = std::time::Instant::now();
        assert_eq!(service.find("two", None).await, Err(Unfound::Busy));
        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn the_box_round_a_place_is_as_wide_as_it_is_high_on_the_ground() {
        let kyiv = LatLon::new(50.45, 30.52).unwrap();
        assert_eq!(view_round(kyiv), "29.7348,49.9500,31.3052,50.9500");
        // On the equator a degree is a degree either way.
        let quito = LatLon::new(0.0, -78.5).unwrap();
        assert_eq!(view_round(quito), "-79.0000,-0.5000,-78.0000,0.5000");
        // It stays on the globe: near a pole, and at the date line.
        let north = LatLon::new(89.9, 0.0).unwrap();
        assert_eq!(view_round(north), "-180.0000,89.4000,180.0000,90.0000");
        let fiji = LatLon::new(-17.0, 179.9).unwrap();
        assert!(view_round(fiji).ends_with(",180.0000,-16.5000"));
    }

    /// In the place of a service, for a bot that has none.
    struct Never;

    impl Geocoder for Never {
        fn is_on(&self) -> bool {
            true
        }

        async fn find(&self, _: &str, _: Option<LatLon>) -> Result<Vec<Found>, Unfound> {
            Err(Unfound::Busy)
        }
    }

    #[tokio::test]
    async fn a_bot_that_was_given_no_service_has_none() {
        let none: Option<Never> = None;
        assert!(!none.is_on());
        assert_eq!(none.find("anywhere", None).await, Ok(Vec::new()));

        let some = Some(Never);
        assert!(some.is_on());
        assert_eq!(some.find("anywhere", None).await, Err(Unfound::Busy));
    }
}
