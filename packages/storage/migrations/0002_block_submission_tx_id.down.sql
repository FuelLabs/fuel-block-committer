ALTER TABLE l1_blob_transaction RENAME TO l1_transactions;

ALTER TABLE l1_fuel_block_submission
    DROP CONSTRAINT IF EXISTS fk_final_tx_id;
    
DROP TABLE IF EXISTS l1_transaction;

DROP TABLE IF EXISTS l1_fuel_block_submission;
