BEGIN;

CREATE TABLE IF NOT EXISTS l1_fuel_block_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    submittal_height    BIGINT NOT NULL CHECK (submittal_height >= 0),
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_pending_transaction (
    transaction_hash BYTEA PRIMARY KEY NOT NULL,
    CHECK (octet_length(transaction_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_state_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_state_fragment (
    fuel_block_hash  BYTEA NOT NULL REFERENCES l1_state_submission(fuel_block_hash) ON DELETE CASCADE,
    fragment_index   BIGINT NOT NULL,
    raw_data         BYTEA NOT NULL,
    completed        BOOLEAN NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    transaction_hash BYTEA REFERENCES l1_pending_transaction(transaction_hash) ON DELETE SET NULL,
    PRIMARY KEY (fuel_block_hash, fragment_index),
    CHECK (octet_length(fuel_block_hash) = 32),
    CHECK (fragment_index >= 0)
);

COMMIT;
