BEGIN;

-- Step 1: Add the finalized_at column to the l1_transactions table
ALTER TABLE l1_transactions
ADD COLUMN finalized_at TIMESTAMPTZ;

-- Step 2: Set finalized_at for existing finalized transactions
UPDATE l1_transactions
SET finalized_at = CURRENT_TIMESTAMP
WHERE state = 1;

-- Step 3: Add a constraint to ensure finalized transactions have finalized_at set
ALTER TABLE l1_transactions
ADD CONSTRAINT check_finalized_at_set
CHECK (state != 1 OR finalized_at IS NOT NULL);

-- Step 4: Add the data column as NULLABLE to the l1_submissions table
ALTER TABLE l1_submissions
ADD COLUMN data BYTEA;

-- Step 5: Reassemble and populate data from l1_fragments into l1_submissions
WITH fragment_data AS (
    SELECT 
        f.submission_id, 
        string_agg(f.data, NULL ORDER BY f.fragment_idx) AS full_data
    FROM l1_fragments f
    GROUP BY f.submission_id
)
UPDATE l1_submissions
SET data = (
    SELECT full_data
    FROM fragment_data
    WHERE l1_submissions.id = fragment_data.submission_id
);

-- Step 6: Drop the old l1_transaction_fragments and l1_fragments tables
DROP TABLE IF EXISTS l1_transaction_fragments;
DROP TABLE IF EXISTS l1_fragments;

-- Step 7: Set the data column to NOT NULL now that all rows are populated
ALTER TABLE l1_submissions
ALTER COLUMN data SET NOT NULL;

COMMIT;
