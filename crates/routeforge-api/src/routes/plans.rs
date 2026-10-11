//! Plans: turning the pending orders of a day into routes. For operators and admins.
//!
//! Asking for a plan stores it and puts one job on the queue, in one transaction (D2).
//! The solvers do the rest, and the plan is read here to see how far they have got.
//! A plan in progress can be cancelled; one that is done can be discarded, which gives
//! its orders back so that the day can be planned again (D30).

use actix_web::{HttpResponse, get, post, web};
use routeforge_domain::db::{depots, plans, queue, routes};
use routeforge_domain::{NewPlan, Plan, PlanId, PlanRequest, PlanStatus, Route, RouteBudget};
use utoipa_actix_web::service_config::ServiceConfig;
use uuid::Uuid;

use crate::app::{AppState, NotAcceptable, NotAllowed, NotLoggedIn, NotNow, NotThere};
use crate::auth::Staff;
use crate::error::ApiError;
use crate::request_id::RequestId;
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(create_plan)
        .service(get_plan)
        .service(list_plan_routes)
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
/// A day has one plan in progress at a time. The `X-Request-Id` of this request is
/// kept on the plan's jobs, so the solvers' logs can be searched for it.
#[utoipa::path(
    tag = "plans",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The plan, queued", body = Plan),
        NotLoggedIn,
        NotAllowed,
        (status = 409, description = "The day already has a plan in progress, or no depot has been set",
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
    use std::time::Duration;

    use actix_web::http::StatusCode;
    use routeforge_domain::db::queue::JOBS_CHANNEL;
    use routeforge_domain::{JobPayload, JobStatus, OrderStatus, Role};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use sqlx::postgres::PgListener;

    use super::*;
    use crate::request_id::X_REQUEST_ID;
    use crate::testing::{self, Actor, Api, date};

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

        for path in [nothing.clone(), format!("{nothing}/routes")] {
            let reply = api.get(Some(&operator), &path).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        }
        for action in ["cancel", "discard"] {
            let reply = api
                .post(Some(&operator), &format!("{nothing}/{action}"), Value::Null)
                .await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{action}");
        }
    }
}
