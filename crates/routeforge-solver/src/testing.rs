//! Shared by the tests: the rows a plan needs, workers that run inside a test, and
//! the check the whole system is judged by.

use std::iter;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use rand::rngs::SmallRng;
use rand::{RngExt, SeedableRng};
use routeforge_domain::db::{couriers, depots, orders, plans, routes, users};
use routeforge_domain::{
    Courier, Depot, DistanceProvider, DistanceTable, Haversine, LatLon, NewCourier, NewDepot,
    NewOrder, NewPlan, NewUser, Order, OrderId, OrderKind, OrderStatus, PlanId, PlanStatus, Role,
    RouteBudget, RouteStop, User, UserId, WorkerId,
};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::wake::{self, Wake};
use crate::worker::{Context, Worker};

/// A lease no test outlives.
pub(crate) const LONG: Duration = Duration::from_secs(600);

/// A day in October 2026.
pub(crate) fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

/// A budget of candidate moves. With it the same seed gives the same route on any
/// machine, however busy, so this is what a test uses unless it needs a search that is
/// still running when something happens to it.
pub(crate) fn moves(iterations: u64) -> RouteBudget {
    RouteBudget::new(None, Some(iterations)).unwrap()
}

/// A budget of time.
pub(crate) fn millis(time_ms: u32) -> RouteBudget {
    RouteBudget::new(Some(time_ms), None).unwrap()
}

/// Settings that keep a test short: brief pauses, and a lease that is long beside them.
pub(crate) fn config() -> Config {
    Config {
        concurrency: 1,
        lease: Duration::from_secs(2),
        heartbeat: Duration::from_millis(100),
        poll: Duration::from_millis(100),
        progress: Duration::from_millis(50),
        roads: None,
    }
}

pub(crate) fn context(pool: &PgPool, config: Config) -> Context<Haversine> {
    Context {
        pool: pool.clone(),
        config,
        distances: Haversine,
    }
}

/// Waits for something that must not take long, so that a test fails instead of
/// hanging.
pub(crate) async fn soon<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("this should not have taken so long")
}

/// Looks every 20 ms until `look` finds something. Gives up after half a minute.
pub(crate) async fn eventually<T>(what: &str, mut look: impl AsyncFnMut() -> Option<T>) -> T {
    let give_up = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(found) = look().await {
            return found;
        }
        assert!(Instant::now() < give_up, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Taken by the tests that depend on time, so that their searches do not run side by
/// side and starve each other's heartbeats of processor time.
pub(crate) async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    TURN.lock().await
}

/// What every plan in a test needs: a depot, the people, and the couriers.
pub(crate) struct City {
    pub(crate) depot: Depot,
    pub(crate) operator: User,
    pub(crate) client: User,
    pub(crate) couriers: Vec<Courier>,
}

/// A depot in Kyiv and one courier for each capacity given.
pub(crate) async fn city(pool: &PgPool, capacities: &[u32]) -> City {
    let user = async |role: Role| {
        let new = NewUser {
            email: format!("{}@example.com", UserId::generate()),
            password_hash: "$argon2id$not-a-real-hash".to_owned(),
            role,
        };
        users::create(pool, &new).await.unwrap()
    };
    let depot = NewDepot {
        name: "Depot".to_owned(),
        location: LatLon::new(50.45, 30.52).unwrap(),
    };
    let mut vans = Vec::new();
    for (number, &capacity) in capacities.iter().enumerate() {
        let new = NewCourier {
            name: format!("Van {}", number + 1),
            capacity,
        };
        vans.push(couriers::create(pool, &new).await.unwrap());
    }
    City {
        depot: depots::set(pool, &depot).await.unwrap(),
        operator: user(Role::Operator).await,
        client: user(Role::Client).await,
        couriers: vans,
    }
}

impl City {
    /// One delivery of the given demand, a kilometre or so north-east of the depot.
    pub(crate) async fn order(&self, pool: &PgPool, date: NaiveDate, demand: u32) -> Order {
        self.order_at(pool, date, (0.01, 0.01), OrderKind::Delivery, demand)
            .await
    }

    /// `count` deliveries of demand 1, scattered up to five kilometres from the depot.
    /// The same seed gives the same places, whatever the date.
    pub(crate) async fn orders(
        &self,
        pool: &PgPool,
        date: NaiveDate,
        count: usize,
        seed: u64,
    ) -> Vec<Order> {
        self.scatter(pool, date, count, seed, 1, false).await
    }

    /// Like [`orders`](Self::orders), as a day's orders are: with demands of 1 to 3,
    /// and one in three a pickup.
    pub(crate) async fn mixed_orders(
        &self,
        pool: &PgPool,
        date: NaiveDate,
        count: usize,
        seed: u64,
    ) -> Vec<Order> {
        self.scatter(pool, date, count, seed, 3, true).await
    }

    async fn scatter(
        &self,
        pool: &PgPool,
        date: NaiveDate,
        count: usize,
        seed: u64,
        largest: u32,
        with_pickups: bool,
    ) -> Vec<Order> {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut placed = Vec::new();
        for _ in 0..count {
            // Degrees north and east of the depot: five kilometres each way at most.
            let off = (
                rng.random_range(-0.045..0.045),
                rng.random_range(-0.07..0.07),
            );
            let demand = rng.random_range(1..=largest);
            let kind = if with_pickups && rng.random_range(0..3) == 0 {
                OrderKind::Pickup
            } else {
                OrderKind::Delivery
            };
            placed.push(self.order_at(pool, date, off, kind, demand).await);
        }
        placed
    }

    /// An order so many degrees north and east of the depot.
    pub(crate) async fn order_at(
        &self,
        pool: &PgPool,
        date: NaiveDate,
        (north, east): (f64, f64),
        kind: OrderKind,
        demand: u32,
    ) -> Order {
        let depot = self.depot.location;
        let new = NewOrder {
            client_id: self.client.id,
            kind,
            service_date: date,
            location: LatLon::new(depot.lat() + north, depot.lon() + east).unwrap(),
            address: None,
            demand,
        };
        orders::create(pool, &new).await.unwrap()
    }

    /// What a plan of this city's orders for `date` needs. The seed is always the same.
    pub(crate) fn new_plan(&self, date: NaiveDate, budget: RouteBudget) -> NewPlan {
        NewPlan {
            service_date: date,
            depot_id: self.depot.id,
            seed: 2026,
            budget,
            created_by: self.operator.id,
        }
    }
}

/// Workers running inside a test, on the test's pool, with one listener between them
/// as in a solver process. Their distances are great circles unless a test gives
/// them others.
pub(crate) struct Fleet<P = Haversine> {
    context: Arc<Context<P>>,
    wake: Wake,
    listener: JoinHandle<()>,
    stop: watch::Sender<bool>,
    workers: Vec<(WorkerId, JoinHandle<()>)>,
}

impl Fleet {
    /// The listener, and no worker yet.
    pub(crate) fn new(pool: &PgPool, config: Config) -> Self {
        Self::with_distances(pool, config, Haversine)
    }

    pub(crate) fn with_workers(pool: &PgPool, config: Config, workers: usize) -> Self {
        let mut fleet = Self::new(pool, config);
        for _ in 0..workers {
            fleet.start_worker();
        }
        fleet
    }
}

impl<P> Fleet<P>
where
    P: DistanceProvider + Send + Sync + 'static,
{
    /// The listener, and no worker yet, with the distances that `distances` gives.
    pub(crate) fn with_distances(pool: &PgPool, config: Config, distances: P) -> Self {
        let (wake, listener) = wake::listen(pool.clone());
        let (stop, _) = watch::channel(false);
        let context = Context {
            pool: pool.clone(),
            config,
            distances,
        };
        Self {
            context: Arc::new(context),
            wake,
            listener,
            stop,
            workers: Vec::new(),
        }
    }

    /// Starts one more worker and says which.
    pub(crate) fn start_worker(&mut self) -> WorkerId {
        let worker = Worker::new(
            Arc::clone(&self.context),
            "test".into(),
            self.wake.clone(),
            self.stop.subscribe(),
        );
        let id = worker.id();
        self.workers.push((id, tokio::spawn(worker.run())));
        id
    }

    /// Ends a worker the way `docker kill` ends a solver: at once, with no chance to
    /// give its job back or to say goodbye.
    pub(crate) async fn kill(&mut self, id: WorkerId) {
        let at = self
            .workers
            .iter()
            .position(|(worker, _)| *worker == id)
            .expect("the fleet has no such worker");
        let (_, task) = self.workers.swap_remove(at);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    /// Tells the workers to stop, as a signal tells a solver, and waits until they
    /// have.
    pub(crate) async fn stop(self) {
        self.stop.send_replace(true);
        for (_, task) in self.workers {
            soon(task).await.unwrap();
        }
        self.listener.abort();
        let _ = self.listener.await;
    }
}

/// The most a courier has on board along a route, reckoned here, apart from the
/// solver: it leaves the depot with every delivery, each delivery comes off at its
/// stop, and each pickup comes on at its own (D65).
pub(crate) fn most_on_board(stops: &[RouteStop]) -> u64 {
    let of = |kind: OrderKind| {
        stops
            .iter()
            .filter(move |stop| stop.kind == kind)
            .map(|stop| u64::from(stop.demand))
            .sum::<u64>()
    };
    let mut on_board = of(OrderKind::Delivery);
    let mut most = on_board;
    for stop in stops {
        match stop.kind {
            OrderKind::Delivery => on_board -= u64::from(stop.demand),
            OrderKind::Pickup => on_board += u64::from(stop.demand),
        }
        most = most.max(on_board);
    }
    most
}

/// What the system is judged by. The plan is done, every one of `expected` is on
/// exactly one of its routes and nothing else is, no courier has more on board at any
/// stop than it can carry, and every route is as long as it says.
pub(crate) async fn assert_routed_once(
    pool: &PgPool,
    city: &City,
    plan_id: PlanId,
    expected: &[Order],
) {
    let plan = plans::get(pool, plan_id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Done);
    let stored = routes::list_for_plan(pool, plan_id).await.unwrap();
    assert_eq!(
        (plan.routes_total as usize, plan.routes_done as usize),
        (stored.len(), stored.len())
    );

    let mut called_at = Vec::new();
    for route in &stored {
        let courier = city
            .couriers
            .iter()
            .find(|courier| courier.id == route.courier_id)
            .expect("a route is for a courier of the city");
        let load = most_on_board(&route.stops);
        assert!(
            load <= u64::from(courier.capacity),
            "a load of {load} on a courier that carries {}",
            courier.capacity
        );
        assert_eq!(
            route.distance_m,
            round_trip(city.depot.location, &route.stops)
        );
        called_at.extend(route.stops.iter().map(|stop| stop.order_id));
    }
    called_at.sort_unstable();
    let mut wanted: Vec<OrderId> = expected.iter().map(|order| order.id).collect();
    wanted.sort_unstable();
    assert_eq!(called_at, wanted, "every order is on exactly one route");

    for order in expected {
        let stored = orders::get(pool, order.id).await.unwrap().unwrap();
        assert_eq!(
            (stored.status, stored.plan_id),
            (OrderStatus::Planned, Some(plan_id))
        );
    }
}

/// How far it is from the depot through the stops and back, by the table the solver
/// uses but reckoned here, apart from it.
pub(crate) fn round_trip(depot: LatLon, stops: &[RouteStop]) -> u64 {
    let positions: Vec<LatLon> = iter::once(depot)
        .chain(stops.iter().map(|stop| stop.location))
        .collect();
    let table = DistanceTable::haversine(&positions);
    let nodes = positions.len();
    (0..nodes)
        .map(|from| u64::from(table.get(from, (from + 1) % nodes)))
        .sum()
}
