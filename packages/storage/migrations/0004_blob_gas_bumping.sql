DELETE FROM l1_blob_transaction;

ALTER TABLE l1_blob_transaction
    ADD COLUMN nonce           BIGINT NOT NULL,
    ADD COLUMN max_fee         NUMERIC(39, 0) NOT NULL,
    ADD COLUMN priority_fee    NUMERIC(39, 0) NOT NULL,
    ADD COLUMN blob_fee        NUMERIC(39, 0) NOT NULL,
    ADD COLUMN created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP;
