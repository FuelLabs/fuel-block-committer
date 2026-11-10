\set ON_ERROR_STOP on
-- psql -v ON_ERROR_STOP=1 -v writer=... -v generation=0 -v fuel_chain_id=... -f scripts/fcr-initialize.sql
BEGIN;
SELECT pg_try_advisory_xact_lock(hashtextextended('fcr:' || :'writer', 0)) AS stopped \gset
\if :stopped
INSERT INTO fcr_safety(writer,state) VALUES (:'writer', jsonb_build_object(
    'writer_identity', :'writer', 'generation', :'generation', 'fuel_chain_id', :'fuel_chain_id',
    'incident', NULL, 'height', NULL, 'hash', NULL, 'progressed_at', 0, 'recoveries', '[]'::jsonb));
COMMIT;
\else
ROLLBACK;
DO $$ BEGIN RAISE EXCEPTION 'Stop this committer writer before initializing safety state'; END $$;
\endif
