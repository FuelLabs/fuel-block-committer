BEGIN;

-- Step 1: Drop the l1_transaction_fragments table
DROP TABLE IF EXISTS l1_transaction_fragments;

-- Step 2: Delete all previous data from l1_fragments and l1_submissions
DELETE FROM l1_fragments;
DELETE FROM l1_submissions;

ALTER TABLE l1_submissions
ADD COLUMN data BYTEA NOT NULL;

-- Step 4: Add columns for tracking blob ranges and Ethereum transaction (now tx_id)
ALTER TABLE l1_fragments
DROP COLUMN fragment_idx,       -- Remove fragment index if no longer needed
DROP COLUMN data,
ADD COLUMN start_byte INTEGER NOT NULL CHECK(start_byte >=0),
ADD COLUMN end_byte INTEGER NOT NULL CHECK(end_byte >= start_byte),
ADD COLUMN tx_id INTEGER NOT NULL  REFERENCES l1_transactions(id) ON DELETE CASCADE;

-- Step 6: Set finalized_at column in l1_transactions table
ALTER TABLE l1_transactions
ADD COLUMN finalized_at TIMESTAMPTZ;

-- Step 7: Set finalized_at for existing finalized transactions
UPDATE l1_transactions
SET finalized_at = CURRENT_TIMESTAMP
WHERE state = 1;

-- Step 8: Add a constraint to ensure finalized transactions have finalized_at set
ALTER TABLE l1_transactions
ADD CONSTRAINT check_finalized_at_set
CHECK (state != 1 OR finalized_at IS NOT NULL);

COMMIT;

