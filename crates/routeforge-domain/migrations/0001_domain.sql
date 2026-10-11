-- Who uses the system, what is to be carried, and what was planned.
--
-- Conventions, here and in every later migration (D19):
--
-- * Ids are UUIDv7 made by the application (D11), so no column default generates one.
-- * An enumeration is text with a CHECK. The values are the ones the Rust types of
--   routeforge-domain write, and a test inserts each of them.
-- * PostgreSQL has no unsigned integers. A value that is a u32 in Rust is a BIGINT
--   with a range check, so everything the type can hold can be stored.
-- * Timestamps are taken from the database clock.

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    email         TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT users_email_key UNIQUE (email),
    -- One address is one account, however it was typed.
    CONSTRAINT users_email_lower_case_check CHECK (email = lower(email)),
    CONSTRAINT users_role_check CHECK (role IN ('client', 'operator', 'admin'))
);

-- Where every route starts and ends (D1). A row is never updated: moving the depot
-- inserts a new one, the newest row is the current depot, and a plan keeps pointing at
-- the one it was made for (D21).
CREATE TABLE depots (
    id         UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    lat        DOUBLE PRECISION NOT NULL,
    lon        DOUBLE PRECISION NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A NaN is in no range, so it is refused as well.
    CONSTRAINT depots_position_check CHECK (lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180)
);

CREATE TABLE couriers (
    id         UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    -- How much demand one route of the courier may carry (D7).
    capacity   BIGINT NOT NULL,
    -- Only active couriers get routes. A courier with routes cannot be deleted.
    active     BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT couriers_capacity_check CHECK (capacity BETWEEN 1 AND 4294967295)
);

-- One planning run for a service date (D2).
CREATE TABLE plans (
    id                UUID PRIMARY KEY,
    service_date      DATE NOT NULL,
    depot_id          UUID NOT NULL REFERENCES depots (id),
    status            TEXT NOT NULL DEFAULT 'queued',
    -- What every random choice in the plan's routes derives from (D5).
    seed              BIGINT NOT NULL,
    created_by        UUID NOT NULL REFERENCES users (id),
    -- Set when the plan is clustered: the pending orders of the date, how many of them
    -- fit no courier, and how many routes the rest was split into.
    orders_total      BIGINT NOT NULL DEFAULT 0,
    orders_unassigned BIGINT NOT NULL DEFAULT 0,
    routes_total      BIGINT NOT NULL DEFAULT 0,
    -- Incremented by each route job in the transaction that completes it. The update
    -- locks the row, so two jobs finishing at once count one after the other.
    routes_done       BIGINT NOT NULL DEFAULT 0,
    -- Why the plan failed.
    error             TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at       TIMESTAMPTZ,
    CONSTRAINT plans_status_check CHECK (status IN ('queued', 'routing', 'done', 'failed', 'cancelled')),
    CONSTRAINT plans_seed_check CHECK (seed BETWEEN 0 AND 4294967295),
    CONSTRAINT plans_counts_check CHECK (
        orders_total BETWEEN 0 AND 4294967295
        AND orders_unassigned BETWEEN 0 AND orders_total
        AND routes_total BETWEEN 0 AND 4294967295
        AND routes_done BETWEEN 0 AND routes_total
    ),
    -- A plan is done only when every one of its routes is stored.
    CONSTRAINT plans_done_check CHECK (status <> 'done' OR routes_done = routes_total),
    CONSTRAINT plans_finished_check CHECK ((status IN ('done', 'failed', 'cancelled')) = (finished_at IS NOT NULL))
);

-- A service date has at most one plan in progress.
CREATE UNIQUE INDEX plans_one_active_per_date ON plans (service_date) WHERE status IN ('queued', 'routing');

-- One parcel to deliver or to pick up: one stop on a route (D1).
CREATE TABLE orders (
    id           UUID PRIMARY KEY,
    client_id    UUID NOT NULL REFERENCES users (id),
    kind         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    -- The day the courier is to call.
    service_date DATE NOT NULL,
    lat          DOUBLE PRECISION NOT NULL,
    lon          DOUBLE PRECISION NOT NULL,
    -- For people to read. Routing uses the position alone.
    address      TEXT,
    -- How much of a courier's capacity the order takes.
    demand       BIGINT NOT NULL,
    -- The plan that gave the order to a courier.
    plan_id      UUID REFERENCES plans (id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT orders_kind_check CHECK (kind IN ('delivery', 'pickup')),
    CONSTRAINT orders_status_check CHECK (status IN ('pending', 'planned', 'cancelled')),
    CONSTRAINT orders_position_check CHECK (lat BETWEEN -90 AND 90 AND lon BETWEEN -180 AND 180),
    CONSTRAINT orders_demand_check CHECK (demand BETWEEN 1 AND 4294967295),
    -- An order is planned exactly while a plan holds it. A plan that fails or is
    -- cancelled gives its orders back as pending (D20).
    CONSTRAINT orders_planned_check CHECK ((status = 'planned') = (plan_id IS NOT NULL))
);

CREATE INDEX orders_service_date_status_idx ON orders (service_date, status);
CREATE INDEX orders_client_id_idx ON orders (client_id);
CREATE INDEX orders_plan_id_idx ON orders (plan_id) WHERE plan_id IS NOT NULL;

-- One courier's round trip from the depot. The row is written when its route job
-- completes; a plan that fails or is cancelled deletes its routes.
CREATE TABLE routes (
    id         UUID PRIMARY KEY,
    plan_id    UUID NOT NULL REFERENCES plans (id),
    courier_id UUID NOT NULL REFERENCES couriers (id),
    -- The length of the round trip in metres (D4).
    distance_m BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A courier has one route in a plan. A route job that completed twice would trip this.
    CONSTRAINT routes_plan_courier_key UNIQUE (plan_id, courier_id),
    CONSTRAINT routes_distance_check CHECK (distance_m >= 0)
);

CREATE TABLE route_stops (
    route_id UUID NOT NULL REFERENCES routes (id) ON DELETE CASCADE,
    -- Where the stop comes on the route, counted from 0.
    position INTEGER NOT NULL,
    order_id UUID NOT NULL REFERENCES orders (id),
    PRIMARY KEY (route_id, position),
    -- An order is on one route at most, and once (D20).
    CONSTRAINT route_stops_order_key UNIQUE (order_id),
    CONSTRAINT route_stops_position_check CHECK (position >= 0)
);
