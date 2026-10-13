-- The way a route takes along the streets (D73).
--
-- A route that was measured by road was measured along a way, and the routing service
-- that measured it can say which. The solver asks, and the way is kept here for the
-- map: an encoded polyline with six decimal places, from the depot through the stops
-- in calling order and back to the depot.
--
-- It is optional. A route on great circles has none, since there the straight line
-- from stop to stop is the way. A route by road has none if the service did not give
-- it, and neither has any route that was stored before this.

ALTER TABLE routes
    ADD COLUMN path TEXT,
    ADD CONSTRAINT routes_path_check CHECK (path IS NULL OR distances = 'road');
