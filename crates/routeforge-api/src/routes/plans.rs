//! Plans: turning the pending orders of a day into routes. For operators and admins.
//!
//! Asking for a plan stores it and puts one job on the queue, in one transaction (D2).
//! The solvers do the rest, and the plan is read here to see how far they have got.
//! A plan in progress can be cancelled; one that is done can be discarded, which gives
//! its orders back so that the day can be planned again (D30).

use std::time::Duration;

use actix_web::web::Bytes;
use actix_web::{HttpResponse, get, post, web};
use routeforge_domain::db::{depots, plans, queue, routes};
use routeforge_domain::{
    NewPlan, Plan, PlanFilter, PlanId, PlanRequest, PlanStatus, Route, RouteBudget, RouteJob,
};
use utoipa_actix_web::service_config::ServiceConfig;
use uuid::Uuid;

use crate::app::{
    AppState, NotAcceptable, NotAllowed, NotLoggedIn, NotNow, NotThere, TooManyStreams,
};
use crate::auth::Staff;
use crate::error::ApiError;
use crate::events::Signal;
use crate::request_id::RequestId;
use crate::stream::{self, Heard, Pace, Watch};
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(create_plan)
        .service(list_plans)
        .service(get_plan)
        .service(list_plan_routes)
        .service(watch_plan)
        .service(cancel_plan)
        .service(discard_plan);
}

/// Plan a day
///
/// Stores the plan and hands it to the solvers. They take the orders of the day that
/// are pending, give each active courier the ones that fit it, and search for the
/// order to call in. Read the plan again to see it go from `queued` to `routing` to
/// `done`.
///
/// A day has one plan at a time that stands: one in progress, or one that is done.
/// A day whose plan is done is planned again once that plan is discarded, and then
/// afresh, with every order of it (D83). The `X-Request-Id` of this request is kept on
/// the plan's jobs, so the solvers' logs can be searched for it.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The plan, queued", body = Plan),
        NotLoggedIn,
        NotAllowed,
        (status = 409, description = "The day already has a plan in progress, or one that is done and is to be discarded first, or no depot has been set",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
        NotAcceptable,
    )
)]
#[post("/api/plans")]
async fn create_plan(
    Staff(operator): Staff,
    state: web::Data<AppState>,
    request_id: RequestId,
    body: web::Json<PlanRequest>,
) -> Result<HttpResponse, ApiError> {
    let request = body.into_inner();
    let budget = match request.budget {
        Some(budget) => {
            validate::budget(budget, &state.limits)?;
            budget
        }
        None => RouteBudget::new(Some(state.limits.default_budget_ms), None)
            .map_err(ApiError::internal)?,
    };
    let depot = depots::current(&state.pool)
        .await?
        .ok_or_else(|| ApiError::Conflict("no depot has been set to plan from".to_owned()))?;

    let new = NewPlan {
        service_date: request.service_date,
        depot_id: depot.id,
        seed: request.seed.unwrap_or_else(any_seed),
        budget,
        created_by: operator.id,
    };
    let plan = plans::submit(&state.pool, &new, Some(request_id.as_str())).await?;
    Ok(HttpResponse::Created().json(plan))
}

/// A seed for a plan that asks for none. The plan says which it got, so the plan can
/// be run again with it.
fn any_seed() -> u32 {
    // The last bytes of a UUIDv7 are random.
    let [.., a, b, c, d] = *Uuid::now_v7().as_bytes();
    u32::from_be_bytes([a, b, c, d])
}

/// List the plans of a day
///
/// Newest first. A day has one plan in progress at most. The others are the ones it
/// had before: done, discarded, failed or cancelled.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    // Named here as well as in the arguments: utoipa describes a query it finds
    // there only if it is listed.
    params(PlanFilter),
    responses(
        (status = 200, description = "The plans of the day, newest first", body = [Plan]),
        NotLoggedIn,
        NotAllowed,
    )
)]
#[get("/api/plans")]
async fn list_plans(
    _: Staff,
    state: web::Data<AppState>,
    query: web::Query<PlanFilter>,
) -> Result<web::Json<Vec<Plan>>, ApiError> {
    Ok(web::Json(plans::list(&state.pool, &query).await?))
}

/// Read a plan
///
/// With how far it has got: the orders it found, the routes it was split into and
/// how many of them are stored.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The plan", body = Plan),
        NotLoggedIn,
        NotAllowed,
        NotThere,
    )
)]
#[get("/api/plans/{id}")]
async fn get_plan(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Plan>, ApiError> {
    Ok(web::Json(existing(&state, path.into_inner().into()).await?))
}

/// Read the routes of a plan
///
/// Those that are stored so far, each with its stops in the order the courier calls.
/// A plan that failed, was cancelled or was discarded has none.
///
/// A route that was measured by road says the way it takes along the streets as
/// `path`, if the routing service gave it. A route without one is drawn from stop to
/// stop.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The routes of the plan that are stored", body = [Route]),
        NotLoggedIn,
        NotAllowed,
        NotThere,
    )
)]
#[get("/api/plans/{id}/routes")]
async fn list_plan_routes(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Vec<Route>>, ApiError> {
    let plan = existing(&state, path.into_inner().into()).await?;
    Ok(web::Json(
        routes::list_for_plan(&state.pool, plan.id).await?,
    ))
}

/// Watch a plan
///
/// An event stream (`text/event-stream`) that stays open, for showing a plan while
/// the solvers are at it. It opens with the plan as it is, and then sends what
/// changes:
///
/// - `plan`: the plan, as `GET /api/plans/{id}` gives it, whenever its status or its
///   counts have changed. When `routes_done` has grown, one route more can be read
///   from `/api/plans/{id}/routes`.
/// - `jobs`: the plan's route jobs, an array of `RouteJob`, whenever one of them was
///   claimed, stored, retried or stopped.
/// - `progress`: a `RouteProgress`, a few times a second for each route that is being
///   searched for. It says how far the search has got and which route is the best so
///   far.
///
/// A line that starts with a colon is a comment, sent to keep the connection open.
/// The stream ends when the token it was opened with expires. Opened again, it starts
/// with the plan as it is then.
///
/// `EventSource` in a browser cannot send the token. The page reads the stream with
/// `fetch`.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The events, for as long as the stream is read",
            body = String, content_type = "text/event-stream"),
        NotLoggedIn,
        NotAllowed,
        NotThere,
        TooManyStreams,
    )
)]
#[get("/api/plans/{id}/events")]
async fn watch_plan(
    Staff(user): Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let watch = PlanWatch {
        plan_id: path.into_inner().into(),
        sent: None,
    };
    stream::open(state, user, watch).await
}

/// What the stream of one plan shows: the plan, its route jobs, and their searches.
struct PlanWatch {
    plan_id: PlanId,
    /// What was last sent, so that only what has changed since is sent again.
    sent: Option<(Plan, Vec<RouteJob>)>,
}

impl Watch for PlanWatch {
    fn hears(&self, signal: &Signal) -> Heard {
        match signal {
            Signal::Plan(plan_id) if *plan_id == self.plan_id => Heard::Changed,
            Signal::Progress(progress) if progress.plan_id == self.plan_id => {
                stream::frame("progress", progress.as_ref()).map_or(Heard::Nothing, Heard::Frame)
            }
            Signal::Missed => Heard::Changed,
            // Of another plan, or of an order that no plan has taken. What a plan
            // does to its orders it says as a change of its own.
            Signal::Plan(_) | Signal::Progress(_) | Signal::Orders(_) => Heard::Nothing,
        }
    }

    async fn read(&mut self, state: &AppState) -> Result<Vec<Bytes>, ApiError> {
        let plan = existing(state, self.plan_id).await?;
        let jobs: Vec<RouteJob> = queue::list_for_plan(&state.pool, self.plan_id)
            .await?
            .into_iter()
            .filter_map(RouteJob::of)
            .collect();

        let (sent_plan, sent_jobs) = match &self.sent {
            Some((plan, jobs)) => (Some(plan), Some(jobs)),
            None => (None, None),
        };
        let mut events = Vec::new();
        if sent_plan != Some(&plan) {
            events.push(stream::frame("plan", &plan)?);
        }
        if sent_jobs != Some(&jobs) {
            events.push(stream::frame("jobs", &jobs)?);
        }
        self.sent = Some((plan, jobs));
        Ok(events)
    }

    fn unprompted(&self, pace: &Pace) -> Duration {
        pace.plan_every
    }
}

/// Cancel a plan that is in progress
///
/// The solvers stop working on it, the routes it had stored are deleted and its
/// orders are pending again.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The plan, now cancelled", body = Plan),
        NotLoggedIn,
        NotAllowed,
        NotThere,
        NotNow,
    )
)]
#[post("/api/plans/{id}/cancel")]
async fn cancel_plan(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Plan>, ApiError> {
    let id = PlanId::from(path.into_inner());
    let cancelled = queue::cancel_plan(&state.pool, id).await?;
    let plan = existing(&state, id).await?;
    if cancelled {
        return Ok(web::Json(plan));
    }
    Err(ApiError::Conflict(match plan.status {
        PlanStatus::Done => {
            "the plan is done: it can be discarded, and no longer cancelled".to_owned()
        }
        status => format!("the plan is {status} already"),
    }))
}

/// Discard a plan that is done
///
/// Its routes are deleted and its orders are pending again, so the day can be planned
/// afresh. The plan itself stays, as `discarded`.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The plan, now discarded", body = Plan),
        NotLoggedIn,
        NotAllowed,
        NotThere,
        NotNow,
    )
)]
#[post("/api/plans/{id}/discard")]
async fn discard_plan(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Plan>, ApiError> {
    let id = PlanId::from(path.into_inner());
    let discarded = queue::discard_plan(&state.pool, id).await?;
    let plan = existing(&state, id).await?;
    if discarded {
        return Ok(web::Json(plan));
    }
    Err(ApiError::Conflict(if plan.status.is_active() {
        format!(
            "the plan is still {}: it can be cancelled, and not yet discarded",
            plan.status
        )
    } else {
        format!("the plan is {} already", plan.status)
    }))
}

async fn existing(state: &AppState, id: PlanId) -> Result<Plan, ApiError> {
    plans::get(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound("there is no such plan"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use actix_web::http::{StatusCode, header};
    use routeforge_domain::db::queue::JOBS_CHANNEL;
    use routeforge_domain::{
        Distances, JobId, JobPayload, JobStatus, LatLon, NewRoute, OrderStatus, Polyline, Role,
        RouteProgress,
    };
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use sqlx::postgres::PgListener;

    use super::*;
    use crate::error::PROBLEM_JSON;
    use crate::events::BACKLOG;
    use crate::request_id::X_REQUEST_ID;
    use crate::stream::EVENT_STREAM;
    use crate::testing::{self, Actor, Api, Stream, date, soon};

    /// How many plans and how many jobs there are.
    async fn plans_and_jobs(pool: &PgPool) -> (i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM plans), (SELECT count(*) FROM jobs)")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn plan_for(day: u32) -> Value {
        json!({ "service_date": date(day) })
    }

    fn id_of(reply: &testing::Reply) -> PlanId {
        reply.body["id"].as_str().unwrap().parse().unwrap()
    }

    #[test]
    fn a_seed_is_made_for_a_plan_that_asks_for_none() {
        let seeds: std::collections::HashSet<u32> = (0..64).map(|_| any_seed()).collect();
        assert!(seeds.len() > 60, "{} seeds out of 64 differ", seeds.len());
    }

    /// What `plan.md` calls enqueue atomicity: the plan row, its job and the
    /// notification appear together.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_is_stored_with_its_job_and_announced(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        let depot = api.depot().await;
        let mut listener = PgListener::connect_with(&pool).await.unwrap();
        listener.listen(JOBS_CHANNEL).await.unwrap();

        let asked = api.post(Some(&operator), "/api/plans", plan_for(12)).await;

        assert_eq!(asked.status, StatusCode::CREATED);
        assert_eq!(asked.body["status"], "queued");
        assert_eq!(asked.body["service_date"], "2026-10-12");
        assert_eq!(asked.body["depot_id"], json!(depot.id));
        assert_eq!(asked.body["created_by"], json!(operator.user.id));
        // What was not asked for is filled in, and said.
        assert_eq!(
            asked.body["budget"],
            json!({ "time_ms": testing::LIMITS.default_budget_ms, "iterations": null })
        );
        assert!(asked.body["seed"].is_u64());

        let plan_id = id_of(&asked);
        let jobs = queue::list_for_plan(&pool, plan_id).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].payload, JobPayload::Plan { plan_id });
        assert_eq!(jobs[0].status, JobStatus::Queued);
        // The id the response was sent with is the one the solvers will log.
        let request_id = asked.headers.get(X_REQUEST_ID).unwrap().to_str().unwrap();
        assert_eq!(jobs[0].request_id.as_deref(), Some(request_id));

        let heard = tokio::time::timeout(Duration::from_secs(10), listener.recv())
            .await
            .expect("the workers were told")
            .unwrap();
        assert_eq!(heard.payload(), "plan");

        let read = api
            .get(Some(&operator), &format!("/api/plans/{plan_id}"))
            .await;
        assert_eq!(read.body, asked.body);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_request_id_a_client_sends_is_the_one_on_the_job(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;

        let asked = api
            .send(
                testing::request("POST", "/api/plans", Some(&operator))
                    .insert_header((X_REQUEST_ID, "demo-run-7"))
                    .set_json(plan_for(12)),
            )
            .await;

        assert_eq!(asked.status, StatusCode::CREATED);
        assert_eq!(asked.headers.get(X_REQUEST_ID).unwrap(), "demo-run-7");
        let jobs = queue::list_for_plan(&pool, id_of(&asked)).await.unwrap();
        assert_eq!(jobs[0].request_id.as_deref(), Some("demo-run-7"));
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_day_with_a_plan_in_progress_is_not_planned_again(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let first = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(first.status, StatusCode::CREATED);

        let second = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(second.status, StatusCode::CONFLICT);
        let detail = second.body["detail"].as_str().unwrap();
        assert!(detail.contains("plan in progress"), "{detail}");
        // The refusal left nothing behind: no second plan, and no job for one.
        assert_eq!(plans_and_jobs(&pool).await, (1, 1));

        // Another day is free.
        let other = api.post(Some(&operator), "/api/plans", plan_for(13)).await;
        assert_eq!(other.status, StatusCode::CREATED);
    }

    /// A day whose plan is done has given its couriers their round. It is planned again
    /// once that plan is discarded, and the refusal says so (D83).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_day_whose_plan_is_done_is_planned_again_once_it_is_discarded(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let first = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(first.status, StatusCode::CREATED);
        let first_id: Uuid = first.body["id"].as_str().unwrap().parse().unwrap();
        // Done, as the solvers leave a plan that had no orders to route.
        for done in [
            "UPDATE jobs SET status = 'done', finished_at = now() WHERE plan_id = $1",
            "UPDATE plans SET status = 'done', finished_at = now() WHERE id = $1",
        ] {
            sqlx::query(done)
                .bind(first_id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let second = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(second.status, StatusCode::CONFLICT);
        let detail = second.body["detail"].as_str().unwrap();
        assert!(detail.contains("discard it"), "{detail}");
        assert_eq!(plans_and_jobs(&pool).await, (1, 1));

        let discard = format!("/api/plans/{first_id}/discard");
        let discarded = api.post(Some(&operator), &discard, Value::Null).await;
        assert_eq!(discarded.status, StatusCode::OK);
        let third = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(third.status, StatusCode::CREATED, "{}", third.text);
    }

    /// The plan and its job are one transaction: a plan whose job cannot be stored is
    /// not stored either.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_whose_job_cannot_be_queued_is_not_stored(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        sqlx::query("ALTER TABLE jobs ADD CONSTRAINT nothing_gets_in CHECK (false) NOT VALID")
            .execute(&pool)
            .await
            .unwrap();

        let asked = api.post(Some(&operator), "/api/plans", plan_for(12)).await;

        assert_eq!(asked.status, StatusCode::INTERNAL_SERVER_ERROR);
        // What broke is the server's business, and stays in its log.
        assert!(!asked.text.contains("nothing_gets_in"), "{}", asked.text);
        assert_eq!(plans_and_jobs(&pool).await, (0, 0));

        // With the fault gone the same request goes through: nothing blocks the day.
        sqlx::query("ALTER TABLE jobs DROP CONSTRAINT nothing_gets_in")
            .execute(&pool)
            .await
            .unwrap();
        let asked = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(asked.status, StatusCode::CREATED);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_needs_a_depot_and_a_budget_within_the_limits(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;

        let no_depot = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(no_depot.status, StatusCode::CONFLICT);
        let detail = no_depot.body["detail"].as_str().unwrap();
        assert!(detail.contains("no depot"), "{detail}");
        api.depot().await;

        let with = |budget: Value, seed: Value| json!({ "service_date": date(12), "budget": budget, "seed": seed });
        let limits = testing::LIMITS;
        for (budget, field) in [
            (
                json!({ "time_ms": limits.max_budget_ms + 1 }),
                "budget.time_ms",
            ),
            (
                json!({ "iterations": limits.max_budget_iterations + 1 }),
                "budget.iterations",
            ),
        ] {
            let refused = api
                .post(Some(&operator), "/api/plans", with(budget, json!(1)))
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(refused.body["errors"][0]["field"], field);
        }
        // A budget without a limit is no budget (D31), and a seed is a u32.
        for unreadable in [
            with(json!({}), json!(1)),
            with(json!({ "time_ms": null, "iterations": null }), json!(1)),
            with(json!({ "time_ms": 1000 }), json!(-1)),
            with(json!({ "time_ms": 1000 }), json!(4_294_967_296_u64)),
        ] {
            let refused = api.post(Some(&operator), "/api/plans", unreadable).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert_eq!(plans_and_jobs(&pool).await, (0, 0));

        // At the limits it is taken as asked.
        let asked = api
            .post(
                Some(&operator),
                "/api/plans",
                with(
                    json!({
                        "time_ms": limits.max_budget_ms,
                        "iterations": limits.max_budget_iterations,
                    }),
                    json!(u32::MAX),
                ),
            )
            .await;
        assert_eq!(asked.status, StatusCode::CREATED);
        assert_eq!(asked.body["seed"], u32::MAX);
        assert_eq!(
            asked.body["budget"],
            json!({
                "time_ms": limits.max_budget_ms,
                "iterations": limits.max_budget_iterations,
            })
        );
    }

    /// A day with four orders, and a plan asked for it.
    async fn planned_day(api: &Api, operator: &Actor, day: u32) -> (PlanId, Vec<String>) {
        let client = api.actor(Role::Client).await;
        let mut placed = Vec::new();
        for _ in 0..4 {
            placed.push(api.order(&client, day).await);
        }
        let asked = api.post(Some(operator), "/api/plans", plan_for(day)).await;
        assert_eq!(asked.status, StatusCode::CREATED);
        (id_of(&asked), placed)
    }

    /// A route is read with the way it takes along the streets, where it has one
    /// (D73): by road, and given by the routing service.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_is_read_with_the_way_it_takes(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        let depot = api.depot().await;
        let asked = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        let plan_id = id_of(&asked);

        let turn = LatLon::new(50.46, 30.53).unwrap();
        let way = Polyline::through(&[depot.location, turn, depot.location]).unwrap();
        // As a solver stores them: by road with its way, by road without, and as the
        // crow flies.
        for (distances, path) in [
            (Distances::Road, Some(way.clone())),
            (Distances::Road, None),
            (Distances::GreatCircle, None),
        ] {
            let route = NewRoute {
                plan_id,
                courier_id: api.courier(10).await.id,
                distance_m: 2_000,
                distances,
                path,
                stops: Vec::new(),
            };
            routes::insert(&pool, &route).await.unwrap();
        }

        let routes = api
            .get(Some(&operator), &format!("/api/plans/{plan_id}/routes"))
            .await;
        assert_eq!(routes.status, StatusCode::OK);
        let said: Vec<(&Value, &Value)> = routes
            .body
            .as_array()
            .unwrap()
            .iter()
            .map(|route| (&route["distances"], &route["path"]))
            .collect();
        // The way as text, which is what a page decodes, and `null` where there is none.
        assert_eq!(
            said,
            [
                (&json!("road"), &json!(way.as_str())),
                (&json!("road"), &Value::Null),
                (&json!("great_circle"), &Value::Null),
            ]
        );
    }

    /// The flow the phase is done by, with a stand-in for the solver: orders, a plan,
    /// the routes read back, and the plan taken back again.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_is_read_back_as_routes_and_can_be_discarded(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let courier = api.courier(10).await;
        let (plan_id, placed) = planned_day(&api, &operator, 12).await;
        let path = format!("/api/plans/{plan_id}");

        // Nothing is routed until a solver has been at it.
        let routes = api.get(Some(&operator), &format!("{path}/routes")).await;
        assert_eq!(routes.body, json!([]));
        // And a plan that is not done cannot be discarded, only cancelled.
        let early = api
            .post(Some(&operator), &format!("{path}/discard"), Value::Null)
            .await;
        assert_eq!(early.status, StatusCode::CONFLICT);
        let detail = early.body["detail"].as_str().unwrap();
        assert!(detail.contains("still queued"), "{detail}");

        testing::work_off(&pool).await;

        let plan = api.get(Some(&operator), &path).await;
        assert_eq!(plan.body["status"], "done");
        assert_eq!(
            (
                &plan.body["orders_total"],
                &plan.body["routes_total"],
                &plan.body["routes_done"]
            ),
            (&json!(4), &json!(1), &json!(1))
        );
        let routes = api.get(Some(&operator), &format!("{path}/routes")).await;
        assert_eq!(routes.status, StatusCode::OK);
        let route = &routes.body[0];
        assert_eq!(route["plan_id"], json!(plan_id));
        assert_eq!(route["courier_id"], json!(courier.id));
        // The stops in the order the courier calls, which the stand-in made the
        // reverse of the order they were placed in.
        let called: Vec<&str> = route["stops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stop| stop["order_id"].as_str().unwrap())
            .collect();
        let mut expected: Vec<&str> = placed.iter().map(String::as_str).collect();
        expected.reverse();
        assert_eq!(called, expected);
        assert_eq!(route["stops"][0]["kind"], "delivery");
        assert_eq!(
            route["stops"][0]["location"],
            json!({ "lat": 50.4, "lon": 30.6 })
        );

        // A plan that is done is not cancelled.
        let late = api
            .post(Some(&operator), &format!("{path}/cancel"), Value::Null)
            .await;
        assert_eq!(late.status, StatusCode::CONFLICT);

        let discarded = api
            .post(Some(&operator), &format!("{path}/discard"), Value::Null)
            .await;
        assert_eq!(discarded.status, StatusCode::OK);
        assert_eq!(discarded.body["status"], "discarded");
        let routes = api.get(Some(&operator), &format!("{path}/routes")).await;
        assert_eq!(routes.body, json!([]));
        for id in &placed {
            let order = orders_status(&pool, id).await;
            assert_eq!(order, OrderStatus::Pending);
        }
        // Once.
        let again = api
            .post(Some(&operator), &format!("{path}/discard"), Value::Null)
            .await;
        assert_eq!(again.status, StatusCode::CONFLICT);

        // The day is planned afresh, and the new plan gets every order.
        let afresh = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        assert_eq!(afresh.status, StatusCode::CREATED);
        testing::work_off(&pool).await;
        let plan = api
            .get(Some(&operator), &format!("/api/plans/{}", id_of(&afresh)))
            .await;
        assert_eq!(
            (&plan.body["status"], &plan.body["orders_total"]),
            (&json!("done"), &json!(4))
        );
    }

    async fn orders_status(pool: &PgPool, id: &str) -> OrderStatus {
        let order = routeforge_domain::db::orders::get(pool, id.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        order.status
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_in_progress_is_cancelled_and_its_orders_freed(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        api.courier(10).await;
        let (plan_id, placed) = planned_day(&api, &operator, 12).await;
        let path = format!("/api/plans/{plan_id}");
        // Clustered, and its route not yet found.
        testing::work_one(&pool).await;
        assert_eq!(orders_status(&pool, &placed[0]).await, OrderStatus::Planned);

        let cancelled = api
            .post(Some(&operator), &format!("{path}/cancel"), Value::Null)
            .await;
        assert_eq!(cancelled.status, StatusCode::OK);
        assert_eq!(cancelled.body["status"], "cancelled");
        for id in &placed {
            assert_eq!(orders_status(&pool, id).await, OrderStatus::Pending);
        }
        // Nothing is left for a solver to do.
        assert!(!testing::work_one(&pool).await);

        for (action, word) in [
            ("cancel", "cancelled already"),
            ("discard", "cancelled already"),
        ] {
            let late = api
                .post(Some(&operator), &format!("{path}/{action}"), Value::Null)
                .await;
            assert_eq!(late.status, StatusCode::CONFLICT, "{action}");
            let detail = late.body["detail"].as_str().unwrap();
            assert!(detail.contains(word), "{detail}");
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_that_is_not_there_is_missed(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        let nothing = format!("/api/plans/{}", PlanId::generate());

        // A stream of it is refused like the rest, and is no stream that says so.
        let paths = [
            nothing.clone(),
            format!("{nothing}/routes"),
            format!("{nothing}/events"),
        ];
        for path in paths {
            let reply = api.get(Some(&operator), &path).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(reply.content_type.as_deref(), Some(PROBLEM_JSON), "{path}");
        }
        for action in ["cancel", "discard"] {
            let reply = api
                .post(Some(&operator), &format!("{nothing}/{action}"), Value::Null)
                .await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{action}");
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_plans_of_a_day_are_listed_newest_first(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let listed = async |query: &str| {
            api.get(Some(&operator), &format!("/api/plans{query}"))
                .await
        };

        let none = listed("?service_date=2026-10-12").await;
        assert_eq!(none.status, StatusCode::OK);
        assert_eq!(none.body, json!([]));

        // A day has one plan in progress, so the first has to be over for a second.
        let first = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        let cancel = format!("/api/plans/{}/cancel", id_of(&first));
        let cancelled = api.post(Some(&operator), &cancel, Value::Null).await;
        assert_eq!(cancelled.status, StatusCode::OK);
        let second = api.post(Some(&operator), "/api/plans", plan_for(12)).await;
        let elsewhere = api.post(Some(&operator), "/api/plans", plan_for(13)).await;

        let monday = listed("?service_date=2026-10-12").await;
        assert_eq!(monday.status, StatusCode::OK);
        // The newest first, each as it is read by its id.
        assert_eq!(monday.body, json!([second.body, cancelled.body]));
        let tuesday = listed("?service_date=2026-10-13").await;
        assert_eq!(tuesday.body, json!([elsewhere.body]));

        // Which day is not left open: the plans of every day would be a list without end.
        for unreadable in ["", "?service_date=monday", "?day=2026-10-12"] {
            let refused = listed(unreadable).await;
            assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{unreadable}");
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
        }
    }

    /// How long a stream is listened to before it is taken to have nothing to say.
    const QUIET: Duration = Duration::from_millis(300);

    fn events_of(plan_id: PlanId) -> String {
        format!("/api/plans/{plan_id}/events")
    }

    /// Opens the stream of a plan and reads what it opens with: the plan and its jobs.
    async fn watching(api: &Api, who: &Actor, plan_id: PlanId) -> (Stream, Value, Value) {
        let mut stream = api.open(who, &events_of(plan_id)).await;
        let plan = stream.next_of("plan").await;
        let jobs = stream.next_of("jobs").await;
        (stream, plan, jobs)
    }

    /// A plan that has just been asked for, and nothing else.
    async fn queued_plan(api: &Api, operator: &Actor, day: u32) -> PlanId {
        let asked = api.post(Some(operator), "/api/plans", plan_for(day)).await;
        assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.text);
        id_of(&asked)
    }

    /// What the page is fed with while a plan is made, with the stand-in for the solver.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_is_watched_from_the_queue_to_its_routes(pool: PgPool) {
        let api = testing::api(&pool);
        let _listening = api.listen().await;
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let courier = api.courier(10).await;
        let (plan_id, placed) = planned_day(&api, &operator, 12).await;
        let path = format!("/api/plans/{plan_id}");

        // It opens with the plan as it is: queued, and split into nothing yet.
        let (mut stream, plan, jobs) = watching(&api, &operator, plan_id).await;
        assert_eq!(plan, api.get(Some(&operator), &path).await.body);
        assert_eq!(plan["status"], "queued");
        assert_eq!(jobs, json!([]));
        assert!(stream.is_quiet_for(QUIET).await, "nothing has changed");

        // Clustered: the plan has moved on, and has a route job.
        assert!(testing::work_one(&pool).await);
        let plan = stream
            .until("plan", |plan| plan["status"] == "routing")
            .await;
        assert_eq!(plan, api.get(Some(&operator), &path).await.body);
        assert_eq!(
            (&plan["routes_total"], &plan["routes_done"]),
            (&json!(1), &json!(0))
        );
        let jobs = stream.next_of("jobs").await;
        assert_eq!(jobs.as_array().unwrap().len(), 1, "{jobs}");
        let job = &jobs[0];
        assert_eq!(job["courier_id"], json!(courier.id));
        // The orders as the job has them, which is what a search counts its stops by.
        assert_eq!(job["order_ids"], json!(placed));
        assert_eq!(
            (
                &job["status"],
                &job["attempt"],
                &job["worker_id"],
                &job["progress"]
            ),
            (&json!("queued"), &json!(0), &Value::Null, &Value::Null)
        );

        // Its one route found and stored: the plan is done, and so is the job.
        assert!(testing::work_one(&pool).await);
        let plan = stream.until("plan", |plan| plan["status"] == "done").await;
        assert_eq!(plan["routes_done"], 1);
        let jobs = stream.next_of("jobs").await;
        assert_eq!(
            (&jobs[0]["status"], &jobs[0]["attempt"]),
            (&json!("done"), &json!(1))
        );
        assert!(jobs[0]["worker_id"].is_string(), "{jobs}");
        assert!(stream.is_quiet_for(QUIET).await);

        // Discarded, through the API. The plan has changed, and its jobs have not.
        let discard = format!("{path}/discard");
        let discarded = api.post(Some(&operator), &discard, Value::Null).await;
        assert_eq!(discarded.status, StatusCode::OK);
        assert_eq!(stream.next_of("plan").await, discarded.body);
        assert!(stream.is_quiet_for(QUIET).await);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_stream_passes_on_the_searches_of_its_own_plan_and_of_no_other(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let ours = queued_plan(&api, &operator, 12).await;
        let theirs = queued_plan(&api, &operator, 13).await;
        let (mut stream, ..) = watching(&api, &operator, ours).await;
        let search_of = |plan_id: PlanId| RouteProgress {
            plan_id,
            job_id: JobId::generate(),
            attempt: 1,
            iterations: 16_384,
            best_cost_m: 4_200,
            order: Some(vec![1, 0, 2]),
        };

        // Nothing listens to the database in this test. What the listener would pass
        // on is said by hand.
        api.events()
            .send(Signal::Progress(Arc::new(search_of(theirs))));
        api.events().send(Signal::Plan(theirs));
        // Nor is an order that somebody placed, be it for the plan's own day (D63).
        api.events().send(Signal::Orders(date(12)));
        assert!(
            stream.is_quiet_for(QUIET).await,
            "another plan is none of its business"
        );

        let search = search_of(ours);
        api.events()
            .send(Signal::Progress(Arc::new(search.clone())));
        assert_eq!(stream.next_of("progress").await, json!(search));

        // A route too long to be announced comes as its numbers alone.
        let numbers = RouteProgress {
            order: None,
            ..search_of(ours)
        };
        api.events()
            .send(Signal::Progress(Arc::new(numbers.clone())));
        let sent = stream.next_of("progress").await;
        assert_eq!(sent, json!(numbers));
        assert!(sent.get("order").is_none(), "{sent}");
    }

    /// A notification is a hint and can be lost (D26). A stream that may have missed
    /// one reads again.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_stream_that_may_have_missed_something_reads_again(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;

        // The listener was away while the plan was cancelled, and says so when it is
        // back. Here there is none, so nobody tells the stream of the cancel either.
        let plan_id = queued_plan(&api, &operator, 12).await;
        let (mut stream, ..) = watching(&api, &operator, plan_id).await;
        assert!(queue::cancel_plan(&pool, plan_id).await.unwrap());
        assert!(stream.is_quiet_for(QUIET).await, "nobody told it");
        api.events().send(Signal::Missed);
        assert_eq!(stream.next_of("plan").await["status"], "cancelled");
        assert!(
            stream.is_quiet_for(QUIET).await,
            "its jobs are as they were"
        );

        // A reader too slow for all that is said: the channel drops the oldest of
        // what it has not read, and tells it that it did.
        let plan_id = queued_plan(&api, &operator, 13).await;
        let (mut stream, ..) = watching(&api, &operator, plan_id).await;
        assert!(queue::cancel_plan(&pool, plan_id).await.unwrap());
        let another = PlanId::generate();
        for _ in 0..=BACKLOG {
            api.events().send(Signal::Plan(another));
        }
        assert_eq!(stream.next_of("plan").await["status"], "cancelled");
    }

    /// And a stream that is told nothing looks all the same, now and then.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_stream_reads_now_and_then_without_being_told(pool: PgPool) {
        let pace = Pace {
            plan_every: Duration::from_millis(100),
            ..testing::PACE
        };
        let api = testing::api_at(&pool, pace);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let plan_id = queued_plan(&api, &operator, 12).await;
        let (mut stream, ..) = watching(&api, &operator, plan_id).await;

        // What it finds unchanged it does not send again.
        assert!(stream.is_quiet_for(QUIET).await);

        assert!(queue::cancel_plan(&pool, plan_id).await.unwrap());
        assert_eq!(stream.next_of("plan").await["status"], "cancelled");
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_quiet_stream_sends_a_comment_now_and_then(pool: PgPool) {
        let pace = Pace {
            keep_alive: Duration::from_millis(100),
            ..testing::PACE
        };
        let api = testing::api_at(&pool, pace);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let plan_id = queued_plan(&api, &operator, 12).await;

        let mut stream = api.open(&operator, &events_of(plan_id)).await;
        let mut blocks = Vec::new();
        for _ in 0..4 {
            blocks.push(soon(stream.block()).await.expect("the stream goes on"));
        }
        assert!(blocks[0].starts_with("event: plan\ndata: {"), "{blocks:?}");
        assert!(blocks[1].starts_with("event: jobs\ndata: ["), "{blocks:?}");
        assert_eq!(blocks[2..], [": keep-alive", ": keep-alive"]);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_stream_is_sent_as_one_and_is_not_to_be_kept(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let plan_id = queued_plan(&api, &operator, 12).await;

        let opened = api.get(Some(&operator), &events_of(plan_id)).await;
        assert_eq!(opened.status, StatusCode::OK);
        assert_eq!(opened.content_type.as_deref(), Some(EVENT_STREAM));
        assert_eq!(
            opened.headers.get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        assert!(opened.headers.get(X_REQUEST_ID).is_some());
        assert!(
            opened.text.starts_with("event: plan\ndata: {"),
            "{}",
            opened.text
        );
    }

    /// A token is checked when a request arrives, and a stream answers for long after
    /// that. It ends when its token does (D50).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_stream_ends_when_the_token_it_was_opened_with_expires(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let plan_id = queued_plan(&api, &operator, 12).await;
        // Good for this second and the next one.
        let leaving = api.expiring_in(&operator, 1);

        let opened = Instant::now();
        let (mut stream, ..) = watching(&api, &leaving, plan_id).await;
        // One with an hour to go, opened beside it.
        let (mut staying, ..) = watching(&api, &operator, plan_id).await;
        stream.ended().await;
        let open_for = opened.elapsed();
        assert!(
            (Duration::from_secs(1)..Duration::from_secs(5)).contains(&open_for),
            "{open_for:?}"
        );

        // Whoever opens it again with that token is told to log in.
        let refused = api.get(Some(&leaving), &events_of(plan_id)).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
        assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));

        // The other stream is still there, and still listening.
        assert!(queue::cancel_plan(&pool, plan_id).await.unwrap());
        api.events().send(Signal::Plan(plan_id));
        assert_eq!(staying.next_of("plan").await["status"], "cancelled");
    }

    /// The server lets a response finish before it stops, and a stream never does. So
    /// the streams are ended when the API is told to stop.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_streams_end_when_the_api_is_told_to_stop(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        api.depot().await;
        let plan_id = queued_plan(&api, &operator, 12).await;
        let (mut stream, ..) = watching(&api, &operator, plan_id).await;
        let mut stats = api.open(&operator, "/api/system/stats/events").await;
        stats.next_of("stats").await;
        let own = "/api/orders/events?service_date=2026-10-12";
        let mut orders = api.open(&operator, own).await;
        orders.next_of("orders").await;
        assert!(stream.is_quiet_for(QUIET).await);

        let told = Instant::now();
        api.events().close();
        stream.ended().await;
        stats.ended().await;
        orders.ended().await;
        let took = told.elapsed();
        assert!(took < Duration::from_secs(2), "{took:?}");

        // One that is opened while the API is stopping says what there is, and ends.
        let (mut late, plan, _) = watching(&api, &operator, plan_id).await;
        assert_eq!(plan["status"], "queued");
        late.ended().await;
    }
}
