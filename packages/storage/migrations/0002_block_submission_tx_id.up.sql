ALTER TABLE l1_transactions RENAME TO l1_blob_transaction;

DROP TABLE IF EXISTS l1_fuel_block_submission;

CREATE TABLE IF NOT EXISTS l1_fuel_block_submission (
    id                  SERIAL PRIMARY KEY NOT NULL,
    fuel_block_hash     BYTEA NOT NULL,
    fuel_block_height   BIGINT NOT NULL UNIQUE CHECK (fuel_block_height >= 0),
    final_tx_id         INTEGER,
    CHECK (octet_length(fuel_block_hash) = 32),
    UNIQUE (fuel_block_hash)
);

CREATE TABLE IF NOT EXISTS l1_transaction (
    id              SERIAL PRIMARY KEY NOT NULL,
    submission_id   INTEGER NOT NULL,
    hash            BYTEA NOT NULL UNIQUE,
    nonce           BIGINT NOT NULL,
    max_fee         NUMERIC(39, 0) NOT NULL,  -- u128
    priority_fee    NUMERIC(39, 0)  NOT NULL, -- u128
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    state           SMALLINT NOT NULL,
    CHECK (octet_length(hash) = 32),
    CHECK (state IN (0, 1, 2))
);

ALTER TABLE l1_fuel_block_submission
    ADD CONSTRAINT fk_final_tx_id FOREIGN KEY (final_tx_id) REFERENCES l1_transaction(id) ON DELETE SET NULL;

ALTER TABLE l1_transaction
    ADD CONSTRAINT fk_submission_id FOREIGN KEY (submission_id) REFERENCES l1_fuel_block_submission(id) ON DELETE CASCADE;
