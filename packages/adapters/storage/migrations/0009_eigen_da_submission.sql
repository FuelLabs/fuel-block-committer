BEGIN;

CREATE TABLE IF NOT EXISTS eigen_submission (
    id              SERIAL PRIMARY KEY,
    request_id      BYTEA NOT NULL UNIQUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    status          SMALLINT NOT NULL,
    CONSTRAINT eigen_submission_status_check 
        CHECK (status IN (0, 1, 2, 3))
);

CREATE TABLE IF NOT EXISTS eigen_submission_fragments (
    id                    SERIAL PRIMARY KEY,
    submission_id         INTEGER NOT NULL REFERENCES eigen_submission(id) ON DELETE CASCADE,
    fragment_id           INTEGER NOT NULL,
    UNIQUE(submission_id, fragment_id)
);

COMMIT;
