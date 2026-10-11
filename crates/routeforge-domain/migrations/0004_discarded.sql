-- A plan that is done can be discarded (D30).
--
-- Discarding undoes the plan the way a plan that fails or is cancelled undoes itself
-- (D20): its routes are deleted and its orders are pending again. The row stays, as
-- `discarded`, and the date can be planned afresh.
--
-- A discarded plan is over like a done one, so it keeps its finished_at: the moment
-- its last route was stored.

ALTER TABLE plans
    DROP CONSTRAINT plans_status_check,
    DROP CONSTRAINT plans_finished_check;

ALTER TABLE plans
    ADD CONSTRAINT plans_status_check
        CHECK (status IN ('queued', 'routing', 'done', 'failed', 'cancelled', 'discarded')),
    ADD CONSTRAINT plans_finished_check
        CHECK ((status IN ('done', 'failed', 'cancelled', 'discarded')) = (finished_at IS NOT NULL));
