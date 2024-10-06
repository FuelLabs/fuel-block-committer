ALTER TABLE l1_blob_transaction
  DROP CONSTRAINT l1_transactions_state_check;

ALTER TABLE l1_blob_transaction
  ADD CONSTRAINT l1_blob_transaction_state_check 
  CHECK (
    state IN (0, 1, 2, 3) 
    AND (state != 1 OR finalized_at IS NOT NULL)
);