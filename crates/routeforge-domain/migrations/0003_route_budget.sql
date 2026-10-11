-- How long the search for each route of a plan may go on (D31).
--
-- The two limits are those of the search itself (D15): a span of time and a number of
-- candidate moves. A plan has at least one of them, because annealing never ends on
-- its own (D18). With both, whichever is reached first ends the search.

ALTER TABLE plans
    -- Wall-clock time for one route, in milliseconds.
    ADD COLUMN budget_time_ms    BIGINT,
    -- Candidate moves for one route. With this limit alone, the same seed gives the
    -- same route (D5).
    ADD COLUMN budget_iterations BIGINT;

-- A plan from before this migration gets ten seconds a route.
UPDATE plans SET budget_time_ms = 10000;

-- A limit that is not set is NULL, and a NULL passes a range check.
ALTER TABLE plans
    ADD CONSTRAINT plans_budget_check CHECK (
        (budget_time_ms IS NOT NULL OR budget_iterations IS NOT NULL)
        AND budget_time_ms BETWEEN 0 AND 4294967295
        AND budget_iterations >= 0
    );
