ALTER TABLE l1_transactions RENAME TO l1_blob_transaction;

DROP TABLE IF EXISTS l1_fuel_block_submission;

CREATE TABLE IF NOT EXISTS l1_fuel_block_submission (
    id                  SERIAL PRIMARY KEY NOT NULL,
    fuel_block_hash     BYTEA NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    completed           BOOLEAN NOT NULL,
    CHECK (octet_length(fuel_block_hash) = 32),
    UNIQUE (fuel_block_hash)
);

CREATE TABLE IF NOT EXISTS l1_transaction (
    id              SERIAL PRIMARY KEY NOT NULL,
    submission_id   INTEGER NOT NULL REFERENCES l1_fuel_block_submission(id),
    hash            BYTEA NOT NULL UNIQUE,
    nonce           BIGINT NOT NULL,
    max_fee         NUMERIC(39, 0) NOT NULL,  -- u128
    priority_fee    NUMERIC(39, 0)  NOT NULL, -- u128
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finalized_at    TIMESTAMPTZ,
    state           SMALLINT NOT NULL,
    CHECK (octet_length(hash) = 32),
    CHECK (state IN (0, 1, 2, 3) AND (state != 1 OR finalized_at IS NOT NULL))
);
