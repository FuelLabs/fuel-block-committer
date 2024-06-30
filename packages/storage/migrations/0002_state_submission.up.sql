CREATE TABLE IF NOT EXISTS l1_state_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_state_fragment (
    fuel_block_hash BYTEA NOT NULL,
    fragment_index  INTEGER NOT NULL,
    raw_data        BYTEA NOT NULL,
    completed       BOOLEAN NOT NULL,
    PRIMARY KEY (fuel_block_hash, fragment_index)
);

CREATE TABLE IF NOT EXISTS l1_pending_transaction (
    transaction_hash BYTEA PRIMARY KEY NOT NULL
);
