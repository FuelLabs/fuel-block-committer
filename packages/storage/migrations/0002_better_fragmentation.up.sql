BEGIN;

-- Rename 'l1_fuel_block_submission' to 'fuel_blocks' to represent the fuel block only
ALTER TABLE l1_fuel_block_submission
RENAME TO fuel_blocks;

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
ADD COLUMN bundle_id INTEGER REFERENCES bundles(id);

COMMIT;
