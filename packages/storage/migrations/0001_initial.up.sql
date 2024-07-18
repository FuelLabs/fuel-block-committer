BEGIN;

CREATE TABLE IF NOT EXISTS l1_fuel_block_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    submittal_height    BIGINT NOT NULL CHECK (submittal_height >= 0),
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_submissions (
    id                SERIAL PRIMARY KEY,
    fuel_block_hash   BYTEA NOT NULL,
    fuel_block_height BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_fragments (
    id            SERIAL PRIMARY KEY NOT NULL,
    position      BIGINT NOT NULL CHECK (position >= 0),
    submission_id INTEGER NOT NULL REFERENCES l1_submissions(id),
    data          BYTEA NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
);

CREATE TABLE IF NOT EXISTS l1_transactions (
    id    SERIAL PRIMARY KEY,
    hash  BYTEA NOT NULL UNIQUE,
    state SMALLINT NOT NULL,
    CHECK (octet_length(hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_transaction_fragments (
    transaction_id INTEGER NOT NULL REFERENCES transactions(id),
    fragment_id INTEGER NOT NULL REFERENCES fragments(id),
    PRIMARY KEY (transaction_id, fragment_id)
);

COMMIT;
