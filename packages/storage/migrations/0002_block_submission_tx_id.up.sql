ALTER TABLE l1_transactions RENAME TO l1_blob_transaction;

CREATE TABLE IF NOT EXISTS l1_transaction (
    id              SERIAL PRIMARY KEY,
    submission_hash BYTEA NOT NULL REFERENCES l1_fuel_block_submission(fuel_block_hash),
    hash            BYTEA NOT NULL UNIQUE,
    nonce           BIGINT NOT NULL,
    max_fee         NUMERIC NOT NULL,
    priority_fee    NUMERIC NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    state           SMALLINT NOT NULL,
    CHECK (octet_length(submission_hash) = 32),
    CHECK (octet_length(hash) = 32),
    CHECK (state IN (0, 1, 2))
);

ALTER TABLE l1_fuel_block_submission
    DROP COLUMN IF EXISTS completed,
    DROP COLUMN IF EXISTS submittal_height,
    ADD COLUMN final_tx_id INTEGER REFERENCES l1_contract_transaction(id) ON DELETE SET NULL;

