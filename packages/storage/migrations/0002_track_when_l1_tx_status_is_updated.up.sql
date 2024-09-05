BEGIN;

ALTER TABLE l1_transactions
ADD COLUMN finalized_at TIMESTAMPTZ;

-- So that previous data passes the constraint added below
UPDATE l1_transactions
SET finalized_at = CURRENT_TIMESTAMP
WHERE state = 1;

-- All finalized tranasctions must have the finalized_at set
ALTER TABLE l1_transactions
ADD CONSTRAINT check_finalized_at_set
CHECK (state != 1 OR finalized_at IS NOT NULL);

ALTER TABLE l1_fuel_block_submission
ADD COLUMN data BYTEA;

-- Reassemble and populate data from l1_fragments
WITH fragment_data AS (
    SELECT 
        f.submission_id, 
        string_agg(f.data, NULL ORDER BY f.fragment_idx) AS full_data
    FROM l1_fragments f
    GROUP BY f.submission_id
)
UPDATE l1_fuel_block_submission
SET data = (
    SELECT full_data
    FROM fragment_data
    WHERE l1_submissions.id = fragment_data.submission_id
)
FROM l1_submissions
WHERE l1_fuel_block_submission.fuel_block_hash = l1_submissions.fuel_block_hash;

DROP TABLE IF EXISTS l1_transaction_fragments;
DROP TABLE IF EXISTS l1_fragments;

-- Set the data column to NOT NULL now that all rows are populated
ALTER TABLE l1_fuel_block_submission
ALTER COLUMN data SET NOT NULL;


COMMIT;
