BEGIN;

-- Rename 'l1_fuel_block_submission' to 'fuel_blocks' to represent the fuel block only
ALTER TABLE l1_fuel_block_submission
RENAME TO fuel_blocks;

-- Rename 'fuel_block_height' to 'height'
ALTER TABLE fuel_blocks
RENAME COLUMN fuel_block_height TO height;

-- Rename 'fuel_block_hash' to 'hash'
ALTER TABLE fuel_blocks
RENAME COLUMN fuel_block_hash TO hash;

-- Drop 'completed' and 'submittal_height' columns
ALTER TABLE fuel_blocks
DROP COLUMN completed,
DROP COLUMN submittal_height,
ADD COLUMN data BYTEA NOT NULL;

-- Create new 'bundles' table to represent groups of blocks
CREATE TABLE IF NOT EXISTS bundles (
    id          SERIAL PRIMARY KEY
);

-- Create a many-to-many relationship between bundles and blocks
CREATE TABLE IF NOT EXISTS bundle_blocks (
    bundle_id   INTEGER NOT NULL REFERENCES bundles(id),
    block_hash  BYTEA NOT NULL REFERENCES fuel_blocks(hash),
    PRIMARY KEY (bundle_id, block_hash)
);

-- Drop 'submission_id' from 'l1_fragments' and add 'bundle_id'
ALTER TABLE l1_fragments
DROP COLUMN submission_id,
DROP COLUMN created_at,
ADD COLUMN bundle_id INTEGER REFERENCES bundles(id) NOT NULL,
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
