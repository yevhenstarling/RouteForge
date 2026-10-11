-- The job queue (D6, D22) and the registry of the workers that drain it (D27).

CREATE TABLE jobs (
    id               UUID PRIMARY KEY,
    -- Every job belongs to a plan (D2).
    plan_id          UUID NOT NULL REFERENCES plans (id),
    kind             TEXT NOT NULL,
    -- A routeforge_domain::JobPayload. It repeats the kind and the plan, and the check
    -- below keeps the copies equal.
    payload          JSONB NOT NULL,
    status           TEXT NOT NULL DEFAULT 'queued',
    -- How many times the job has been claimed. It is also the fencing token: a worker
    -- may touch the job only while this still has the value it was claimed with.
    attempt          BIGINT NOT NULL DEFAULT 0,
    -- After this many claims a failure is final.
    max_attempts     BIGINT NOT NULL,
    -- From when a queued job may be claimed.
    run_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The worker of the latest claim. Deliberately not a foreign key: see workers.
    claimed_by       UUID,
    -- From when a running job may be taken over by another worker.
    lease_expires_at TIMESTAMPTZ,
    -- A routeforge_domain::JobProgress, as the last heartbeat reported it.
    progress         JSONB,
    -- Why the latest attempt failed.
    error            TEXT,
    -- The id of the HTTP request that asked for the plan, for tracing.
    request_id       TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at      TIMESTAMPTZ,
    CONSTRAINT jobs_kind_check CHECK (kind IN ('plan', 'route')),
    CONSTRAINT jobs_payload_check CHECK (payload ->> 'kind' = kind AND payload ->> 'plan_id' = plan_id::text),
    CONSTRAINT jobs_status_check CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    CONSTRAINT jobs_attempt_check CHECK (max_attempts BETWEEN 1 AND 4294967295 AND attempt BETWEEN 0 AND max_attempts),
    -- A job has a lease exactly while it is running, and then it has a holder.
    CONSTRAINT jobs_lease_check CHECK ((status = 'running') = (lease_expires_at IS NOT NULL)),
    CONSTRAINT jobs_holder_check CHECK (status <> 'running' OR claimed_by IS NOT NULL),
    CONSTRAINT jobs_finished_check CHECK ((status IN ('done', 'failed', 'cancelled')) = (finished_at IS NOT NULL))
);

-- The jobs a worker could claim, oldest first. Finished jobs pile up and stay out of it.
CREATE INDEX jobs_claimable_idx ON jobs (run_at, id) WHERE status IN ('queued', 'running');

CREATE INDEX jobs_plan_id_idx ON jobs (plan_id);

-- The solver processes, for the load view. Each one adds itself when it starts and
-- refreshes last_seen_at while it lives.
--
-- The queue never reads this table. jobs.claimed_by holds a worker's id without
-- referencing it, so a worker that could not register can still work, and old rows
-- can be removed at any time.
CREATE TABLE workers (
    id           UUID PRIMARY KEY,
    hostname     TEXT NOT NULL,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
