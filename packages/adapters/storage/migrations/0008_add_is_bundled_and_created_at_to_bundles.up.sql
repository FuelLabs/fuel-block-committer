BEGIN;

ALTER TABLE fuel_blocks 
  ADD COLUMN IF NOT EXISTS is_bundled BOOLEAN;

UPDATE fuel_blocks fb
SET is_bundled = true
FROM bundles b
WHERE fb.height BETWEEN b.start_height AND b.end_height;

UPDATE fuel_blocks
SET is_bundled = false
WHERE is_bundled IS NULL;

ALTER TABLE fuel_blocks 
  ALTER COLUMN is_bundled SET NOT NULL,
  ALTER COLUMN is_bundled SET DEFAULT false;

CREATE INDEX IF NOT EXISTS idx_fuel_blocks_is_bundled_height 
  ON fuel_blocks(is_bundled, height);

ALTER TABLE bundles
  ADD COLUMN created_at TIMESTAMPTZ;

UPDATE bundles SET created_at = NOW() WHERE created_at IS NULL;

ALTER TABLE bundles
  ALTER COLUMN created_at SET NOT NULL;

CREATE INDEX idx_bundles_created_at ON bundles(created_at);

COMMIT;
