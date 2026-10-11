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
pub struct JobProgress {
    /// Candidate moves evaluated so far.
    pub iterations: u64,
    /// The length of the best route so far, in metres.
    pub best_cost_m: u64,
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

/// A solver process, as it last registered itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}
