CREATE TABLE IF NOT EXISTS l1_state_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    CHECK (octet_length(fuel_block_hash) = 32)
);

CREATE TABLE IF NOT EXISTS l1_state_fragment (
    fuel_block_hash BYTEA,
    fragment_index INTEGER,
    raw_data BYTEA[],
    completed BOOLEAN NOT NULL,
    PRIMARY KEY (fuel_block_hash, fragment_index)
);
