BEGIN;

CREATE TABLE IF NOT EXISTS fuel_blocks (
    hash     BYTEA PRIMARY KEY NOT NULL,
    height   BIGINT NOT NULL UNIQUE CHECK (height >= 0),
    CHECK (octet_length(hash) = 32),
    data BYTEA NOT NULL
);

-- Create new 'bundles' table to represent groups of blocks
CREATE TABLE IF NOT EXISTS bundles (
    id          SERIAL PRIMARY KEY,
    start_height  BIGINT NOT NULL CHECK (start_height >= 0),
    end_height    BIGINT NOT NULL CHECK (end_height >= start_height) -- Ensure valid range
);

CREATE INDEX idx_bundles_start_end ON bundles (start_height, end_height);

ALTER TABLE l1_fragments
DROP COLUMN submission_id,
DROP COLUMN created_at,
ADD COLUMN total_bytes BIGINT NOT NULL CHECK (total_bytes > 0),
ADD COLUMN unused_bytes BIGINT NOT NULL CHECK (unused_bytes >= 0),
ADD COLUMN bundle_id INTEGER REFERENCES bundles(id) NOT NULL,
ADD CONSTRAINT check_data_not_empty CHECK (octet_length(data) > 0),
ALTER COLUMN fragment_idx TYPE INTEGER;

ALTER TABLE l1_fragments
RENAME COLUMN fragment_idx TO idx;


-- Add the new finalized_at column with UTC timestamp, allowing NULL values initially
ALTER TABLE l1_transactions
ADD COLUMN finalized_at TIMESTAMPTZ;

-- Update rows where state is 1 and set finalized_at to the current timestamp
UPDATE l1_transactions
SET finalized_at = NOW()
WHERE state = 1;

-- Add a check constraint to ensure finalized_at is not null when state is 1
ALTER TABLE l1_transactions
ADD CONSTRAINT state_finalized_check
CHECK (state != 1 OR finalized_at IS NOT NULL);


COMMIT;
