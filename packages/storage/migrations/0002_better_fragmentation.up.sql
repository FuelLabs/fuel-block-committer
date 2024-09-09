BEGIN;

-- Rename 'l1_fuel_block_submission' to 'fuel_blocks' to represent the fuel block only
ALTER TABLE l1_fuel_block_submission
RENAME TO fuel_blocks;

-- Rename 'fuel_block_height' to 'height' and 'fuel_block_hash' to 'hash'
ALTER TABLE fuel_blocks
RENAME COLUMN fuel_block_height TO height,
RENAME COLUMN fuel_block_hash TO hash;

-- Drop 'completed' and 'submittal_height' columns
ALTER TABLE fuel_blocks
DROP COLUMN completed,
DROP COLUMN submittal_height;

-- Add 'data' column to store block data
ALTER TABLE fuel_blocks
ADD COLUMN data BYTEA NOT NULL;

-- Create new 'bundles' table to represent groups of blocks
CREATE TABLE IF NOT EXISTS bundles (
    id          SERIAL PRIMARY KEY,
    cancelled   BOOLEAN NOT NULL DEFAULT FALSE -- Boolean flag to indicate if the bundle is cancelled
);

-- Create a many-to-many relationship between bundles and blocks
CREATE TABLE IF NOT EXISTS bundle_blocks (
    bundle_id   INTEGER NOT NULL REFERENCES bundles(id),
    block_hash  BYTEA NOT NULL REFERENCES fuel_blocks(fuel_block_hash),
    PRIMARY KEY (bundle_id, block_hash)
);

-- Add a new 'bundle_id' column to 'l1_fragments' to link fragments to bundles
ALTER TABLE l1_fragments
DROP COLUMN submission_id,
ADD COLUMN bundle_id INTEGER REFERENCES bundles(id);

COMMIT;
