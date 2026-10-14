-- Two reads that run often, and the indexes that serve them (Phase 12).

-- The live view counts the jobs that ended within the last day, beside those that are
-- queued or running, and no longer every job there ever was (D84). The jobs that ended
-- pile up, and this keeps the count to those of the day.
CREATE INDEX jobs_finished_at_idx ON jobs (finished_at) WHERE finished_at IS NOT NULL;

-- The stream of a client's own orders reads that client's orders of one day whenever
-- any plan changes (D77). One index serves that read, and every read by client alone,
-- which the one it takes the place of served.
CREATE INDEX orders_client_id_service_date_idx ON orders (client_id, service_date);
DROP INDEX orders_client_id_idx;
