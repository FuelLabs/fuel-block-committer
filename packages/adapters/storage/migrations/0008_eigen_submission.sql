BEGIN;

CREATE TABLE IF NOT EXISTS eigen_submission (
    id              SERIAL PRIMARY KEY,
    request_id      TEXT NOT NULL UNIQUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finalized_at    TIMESTAMPTZ,
    state           SMALLINT NOT NULL,
    CONSTRAINT eigen_submission_state_check 
        CHECK (state IN (0, 1, 2, 3) AND (state != 1 OR finalized_at IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS eigen_submission_fragments (
    id                    SERIAL PRIMARY KEY,
    submission_id         INTEGER NOT NULL REFERENCES eigen_submission(id) ON DELETE CASCADE,
    fragment_id           INTEGER NOT NULL,
    UNIQUE(submission_id, fragment_id)
);

COMMIT;
