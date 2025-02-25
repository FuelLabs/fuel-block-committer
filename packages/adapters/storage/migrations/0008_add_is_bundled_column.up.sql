BEGIN;

ALTER TABLE fuel_blocks 
  ADD COLUMN IF NOT EXISTS is_bundled BOOLEAN NOT NULL DEFAULT false;

-- Create an index to help queries filter quickly by is_bundled and height.
CREATE INDEX IF NOT EXISTS idx_fuel_blocks_is_bundled_height 
  ON fuel_blocks(is_bundled, height);

COMMIT;
