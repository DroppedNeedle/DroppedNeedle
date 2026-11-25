-- The loop supervisor keeps each loop's liveness in memory, so nothing
-- reads or writes `durable_loops` any more. Its rows were cleared at every
-- start, so dropping it loses nothing.

DROP TABLE durable_loops;

PRAGMA user_version = 173;
