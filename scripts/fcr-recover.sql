\set ON_ERROR_STOP on
-- Offline, after coordinated Fuel/DA/Redis/P2P recovery and pending Ethereum nonce invalidation.
-- Required psql variables: writer, incident, previous_generation, generation, height, hash,
-- recovery_record, old_signatures_invalidated (exactly true).
BEGIN;
SELECT pg_try_advisory_xact_lock(hashtextextended('fcr:' || :'writer', 0)) AS stopped \gset
\if :stopped
WITH recovered AS (
    UPDATE fcr_safety SET state = state || jsonb_build_object(
        'generation', :'generation', 'height', :'height'::bigint, 'hash', :'hash',
        'incident', NULL, 'progressed_at', 0,
        'recoveries', COALESCE(state->'recoveries', '[]'::jsonb) || jsonb_build_array(jsonb_build_object(
            'incident', :'incident', 'previous_generation', :'previous_generation', 'generation', :'generation',
            'height', :'height'::bigint, 'hash', :'hash', 'recovery_record', :'recovery_record', 'at', NOW())))
    WHERE writer=:'writer' AND state->>'incident'=:'incident' AND state->>'generation'=:'previous_generation'
        AND :'generation'<>:'previous_generation' AND length(:'generation')>0
        AND :'old_signatures_invalidated'='true' AND length(:'recovery_record')>0
        AND :'height'::bigint>=0 AND :'hash'~'^0x[0-9a-fA-F]{64}$'
    RETURNING writer
)
SELECT count(*)=1 AS acknowledged FROM recovered \gset
\if :acknowledged
COMMIT;
\else
ROLLBACK;
DO $$ BEGIN RAISE EXCEPTION 'Recovery acknowledgement does not match the active latch, generation and checkpoint requirements'; END $$;
\endif
\else
ROLLBACK;
DO $$ BEGIN RAISE EXCEPTION 'Stop this committer writer before recovering safety state'; END $$;
\endif
