-- A key that makes placing an order safe to repeat (D76).
--
-- Whoever places an order may send a key of their own making along. An order that was
-- placed with one keeps it here. A second request with the same key, from the same
-- client, then finds that order in the index below and is answered with it: nothing
-- is stored a second time.
--
-- It is optional. An order that was placed without a key has none, and so has every
-- order that was stored before this.

ALTER TABLE orders
    ADD COLUMN idempotency_key TEXT,
    -- What `IdempotencyKey` of routeforge-domain takes: a key is never empty, and
    -- never longer than this.
    ADD CONSTRAINT orders_idempotency_key_check CHECK (char_length(idempotency_key) BETWEEN 1 AND 64);

-- A key is a client's own. Two clients who make up the same key have placed two
-- orders, and neither is answered with the other's.
CREATE UNIQUE INDEX orders_client_idempotency_key ON orders (client_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
