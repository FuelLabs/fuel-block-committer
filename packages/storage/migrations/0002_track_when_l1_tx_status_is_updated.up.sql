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

COMMIT;
