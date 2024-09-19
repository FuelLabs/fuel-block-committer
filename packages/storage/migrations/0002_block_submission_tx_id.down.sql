ALTER TABLE l1_blob_transaction RENAME TO l1_transactions;

DROP TABLE IF EXISTS l1_contract_transaction;

ALTER TABLE l1_fuel_block_submission
    DROP COLUMN IF EXISTS tx_id,
    ADD COLUMN completed BOOLEAN;