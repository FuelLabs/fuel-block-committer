BEGIN;

-- 1. Add the column without enforcing NOT NULL initially
ALTER TABLE fuel_blocks 
  ADD COLUMN IF NOT EXISTS is_bundled BOOLEAN;

-- 2. Set is_bundled to true for blocks that fall within any bundle's range.
UPDATE fuel_blocks fb
SET is_bundled = true
FROM bundles b
WHERE fb.height BETWEEN b.start_height AND b.end_height;

-- 3. For blocks not updated above, set is_bundled to false
UPDATE fuel_blocks
SET is_bundled = false
WHERE is_bundled IS NULL;

-- 4. Make the column NOT NULL and set the default for future inserts.
ALTER TABLE fuel_blocks 
  ALTER COLUMN is_bundled SET NOT NULL,
  ALTER COLUMN is_bundled SET DEFAULT false;

-- Create the composite index.
CREATE INDEX IF NOT EXISTS idx_fuel_blocks_is_bundled_height 
  ON fuel_blocks(is_bundled, height);

COMMIT;
