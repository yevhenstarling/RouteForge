-- What the length of a route was measured along (D66).
--
-- A solver that has a routing service to ask searches for a route on road distances,
-- and stores the length of the route by road. For a route the service did not answer
-- for, it falls back on great circles. So the routes of one plan can differ in this,
-- and each says which it is: a length by road is not to be taken for one as the crow
-- flies, nor the other way round.
--
-- Every route that was stored before this was measured along great circles.

ALTER TABLE routes
    ADD COLUMN distances TEXT NOT NULL DEFAULT 'great_circle',
    ADD CONSTRAINT routes_distances_check CHECK (distances IN ('great_circle', 'road'));
