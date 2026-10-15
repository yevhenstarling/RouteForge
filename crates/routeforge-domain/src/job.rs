//! Jobs, the units of work on the queue, and the workers that run them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{CourierId, JobId, OrderId, PlanId, WorkerId};
use crate::text::text_enum;

text_enum! {
    /// The two kinds of work a plan consists of (D2).
    JobKind {
        /// Cluster the plan's orders and enqueue one route job per cluster.
        Plan => "plan",
        /// Put the stops of one courier in a good order.
        Route => "route",
    }
}

text_enum! {
    /// Where a job stands.
    JobStatus {
        /// Waiting for a worker, or for its retry time.
        Queued => "queued",
        /// A worker holds it under a lease.
        Running => "running",
        /// Its results are stored.
        Done => "done",
        /// It ran out of attempts.
        Failed => "failed",
        /// Its plan was cancelled or failed before the job was done.
        Cancelled => "cancelled",
    }
}

/// What a job is to do. Stored as JSONB, with the kind as the `kind` field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobPayload {
    Plan {
        plan_id: PlanId,
    },
    Route {
        plan_id: PlanId,
        courier_id: CourierId,
        /// The courier's orders, as clustering left them. The job finds the order to
        /// call in.
        order_ids: Vec<OrderId>,
        /// Seeds the route search, so the same job gives the same route (D5).
        seed: u64,
    },
}

impl JobPayload {
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Plan { .. } => JobKind::Plan,
            Self::Route { .. } => JobKind::Route,
        }
    }

    /// The plan the job belongs to. Every job has one.
    pub fn plan_id(&self) -> PlanId {
        match *self {
            Self::Plan { plan_id } | Self::Route { plan_id, .. } => plan_id,
        }
    }
}

/// How far a running job has got, as its worker last reported with a heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobProgress {
    /// Candidate moves evaluated so far.
    pub iterations: u64,
    /// The length of the best route so far, in metres.
    pub best_cost_m: u64,
}

/// How far the search for one route has got, as its worker announces it a few times a
/// second while it searches (D48, D49).
///
/// Nothing stores it. The next one replaces it, and one that is lost is not missed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RouteProgress {
    pub plan_id: PlanId,
    pub job_id: JobId,
    /// Which claim of the job this comes from (D22). A worker that has lost the job may
    /// go on announcing until its next heartbeat tells it so. What it says then has a
    /// lower number than the job has.
    pub attempt: u32,
    /// Candidate moves evaluated so far.
    pub iterations: u64,
    /// The length of the best route so far, in metres.
    pub best_cost_m: u64,
    /// The best route so far: its stops in calling order, each as its position in the
    /// `order_ids` of the job. Left out when it would not fit a notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<u32>>,
}

/// A job as the queue stores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub plan_id: PlanId,
    pub payload: JobPayload,
    pub status: JobStatus,
    /// How many times the job has been claimed. Every claim adds one, and a worker's
    /// claim is only valid while this still has the value it was claimed with (D6).
    pub attempt: u32,
    /// After this many claims a failure is final.
    pub max_attempts: u32,
    /// From when a queued job may be claimed.
    pub run_at: DateTime<Utc>,
    /// The worker of the latest claim.
    pub claimed_by: Option<WorkerId>,
    /// From when a running job may be taken over. Set exactly while it is running.
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub progress: Option<JobProgress>,
    /// Why the latest attempt failed.
    pub error: Option<String>,
    /// The id of the HTTP request that asked for the plan, for tracing.
    pub request_id: Option<String>,
    pub created_at: DateTime<Utc>,
    /// When the job was done, failed or cancelled.
    pub finished_at: Option<DateTime<Utc>>,
}

/// A route job as someone watching its plan sees it: whose route it is, which stops it
/// has to put in order, and how the search for that order stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RouteJob {
    pub job_id: JobId,
    pub courier_id: CourierId,
    /// The courier's orders, as clustering left them. The `order` of a `RouteProgress`
    /// counts into this list.
    pub order_ids: Vec<OrderId>,
    pub status: JobStatus,
    /// How many times the job has been claimed.
    pub attempt: u32,
    /// The worker of the latest claim.
    pub worker_id: Option<WorkerId>,
    /// How far the search has got, as the last heartbeat stored it. That is a few
    /// seconds old at most, and what is announced in between is fresher.
    pub progress: Option<JobProgress>,
}

impl RouteJob {
    /// What a watcher sees of `job`. `None` if it is the `plan` job, which has no route.
    pub fn of(job: Job) -> Option<Self> {
        let JobPayload::Route {
            courier_id,
            order_ids,
            ..
        } = job.payload
        else {
            return None;
        };
        Some(Self {
            job_id: job.id,
            courier_id,
            order_ids,
            status: job.status,
            attempt: job.attempt,
            worker_id: job.claimed_by,
            progress: job.progress,
        })
    }
}

/// How many jobs stand in each status.
///
/// `queued` and `running` are the work still to be done. The other three are the jobs
/// that ended within a span: the last day, in what the API gives (D84).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QueueDepth {
    pub queued: u64,
    pub running: u64,
    pub done: u64,
    pub failed: u64,
    pub cancelled: u64,
}

/// A solver process, as it last registered itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkerInfo {
    pub id: WorkerId,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// The job it holds under a lease that has not expired.
    pub current_job: Option<JobId>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::text::testing::assert_text_round_trips;

    #[test]
    fn every_enumeration_has_one_text_everywhere() {
        assert_text_round_trips(JobKind::ALL);
        assert_text_round_trips(JobStatus::ALL);
    }

    #[test]
    fn a_payload_carries_its_kind_as_a_field() {
        let plan_id = PlanId::generate();
        let courier_id = CourierId::generate();
        let order_ids = vec![OrderId::generate(), OrderId::generate()];

        let plan = JobPayload::Plan { plan_id };
        assert_eq!(
            serde_json::to_value(&plan).unwrap(),
            json!({ "kind": "plan", "plan_id": plan_id })
        );

        let route = JobPayload::Route {
            plan_id,
            courier_id,
            order_ids: order_ids.clone(),
            seed: u64::MAX,
        };
        assert_eq!(
            serde_json::to_value(&route).unwrap(),
            json!({
                "kind": "route",
                "plan_id": plan_id,
                "courier_id": courier_id,
                "order_ids": order_ids,
                "seed": u64::MAX,
            })
        );
    }

    #[test]
    fn the_kind_field_is_the_text_of_the_kind() {
        let plan_id = PlanId::generate();
        let payloads = [
            JobPayload::Plan { plan_id },
            JobPayload::Route {
                plan_id,
                courier_id: CourierId::generate(),
                order_ids: Vec::new(),
                seed: 7,
            },
        ];
        for payload in payloads {
            let json = serde_json::to_value(&payload).unwrap();
            assert_eq!(json["kind"], payload.kind().as_str());
            assert_eq!(payload.plan_id(), plan_id);
            assert_eq!(serde_json::from_value::<JobPayload>(json).unwrap(), payload);
        }
    }

    #[test]
    fn a_payload_of_an_unknown_kind_is_refused() {
        let json = json!({ "kind": "teleport", "plan_id": PlanId::generate() });
        assert!(serde_json::from_value::<JobPayload>(json).is_err());
    }

    fn job(payload: JobPayload) -> Job {
        Job {
            id: JobId::generate(),
            plan_id: payload.plan_id(),
            payload,
            status: JobStatus::Running,
            attempt: 2,
            max_attempts: 3,
            run_at: Utc::now(),
            claimed_by: Some(WorkerId::generate()),
            lease_expires_at: Some(Utc::now()),
            progress: Some(JobProgress {
                iterations: 16_384,
                best_cost_m: 4_200,
            }),
            error: Some("the first attempt ran out of memory".to_owned()),
            request_id: Some("req-1".to_owned()),
            created_at: Utc::now(),
            finished_at: None,
        }
    }

    #[test]
    fn a_watcher_sees_a_route_job_as_whose_route_it_is_and_how_it_stands() {
        let plan_id = PlanId::generate();
        let courier_id = CourierId::generate();
        let order_ids = vec![OrderId::generate(), OrderId::generate()];
        let route = job(JobPayload::Route {
            plan_id,
            courier_id,
            order_ids: order_ids.clone(),
            seed: 7,
        });

        let seen = RouteJob::of(route.clone()).unwrap();
        assert_eq!(
            seen,
            RouteJob {
                job_id: route.id,
                courier_id,
                order_ids,
                status: JobStatus::Running,
                attempt: 2,
                worker_id: route.claimed_by,
                progress: route.progress,
            }
        );
        // The seed, the lease and the errors of earlier attempts are the queue's business.
        let json = serde_json::to_value(&seen).unwrap();
        let mut fields: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "attempt",
                "courier_id",
                "job_id",
                "order_ids",
                "progress",
                "status",
                "worker_id"
            ]
        );

        // The `plan` job has no route to show.
        assert_eq!(RouteJob::of(job(JobPayload::Plan { plan_id })), None);
    }

    #[test]
    fn progress_names_its_route_only_when_it_has_one_to_name() {
        let (plan_id, job_id) = (PlanId::generate(), JobId::generate());
        let numbers = RouteProgress {
            plan_id,
            job_id,
            attempt: 2,
            iterations: 32_768,
            best_cost_m: 4_200,
            order: None,
        };
        let json = serde_json::to_value(&numbers).unwrap();
        assert_eq!(
            json,
            json!({
                "plan_id": plan_id,
                "job_id": job_id,
                "attempt": 2,
                "iterations": 32_768,
                "best_cost_m": 4_200,
            })
        );
        assert_eq!(
            serde_json::from_value::<RouteProgress>(json).unwrap(),
            numbers
        );

        let with_route = RouteProgress {
            order: Some(vec![2, 0, 1]),
            ..numbers
        };
        let json = serde_json::to_value(&with_route).unwrap();
        assert_eq!(json["order"], json!([2, 0, 1]));
        assert_eq!(
            serde_json::from_value::<RouteProgress>(json).unwrap(),
            with_route
        );
    }
}
