CREATE TABLE IF NOT EXISTS eth_fuel_block_submission (
    fuel_block_hash     BYTEA PRIMARY KEY NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    submittal_height    BIGINT NOT NULL CHECK (submittal_height >= 0),
    CHECK (octet_length(fuel_block_hash) = 32)
);
