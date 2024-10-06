ALTER TABLE l1_transaction
  DROP CONSTRAINT l1_transaction_check;

ALTER TABLE l1_transaction
  ADD CONSTRAINT l1_transaction_state_check 
  CHECK (
    state IN (0, 1, 2, 3) 
    AND (state != 1 OR finalized_at IS NOT NULL)
);