BEGIN;

CREATE TABLE IF NOT EXISTS bundle_cost (
    bundle_id       INTEGER PRIMARY KEY REFERENCES bundles(id),
    da_block_height BIGINT NOT NULL,          -- DA block height of the last transaction in the bundle
    cost            NUMERIC(39, 0) NOT NULL,
    size            BIGINT NOT NULL,
    is_finalized    BOOLEAN NOT NULL
);

ALTER TABLE bundle_cost
  ADD CONSTRAINT bundle_cost_da_block_height_check 
  CHECK (
    da_block_height >= 0
);

ALTER TABLE bundle_cost
  ADD CONSTRAINT bundle_cost_cost_check 
  CHECK (
    cost >= 0
);

ALTER TABLE bundle_cost
  ADD CONSTRAINT bundle_cost_size_check 
  CHECK (
    size >= 0
);

ALTER TABLE l1_blob_transaction
  DROP CONSTRAINT l1_blob_transaction_state_check;

ALTER TABLE l1_blob_transaction
  ADD CONSTRAINT l1_blob_transaction_state_check 
  CHECK (
    state IN (0, 1, 2, 3, 4) 
    AND (state != 1 OR finalized_at IS NOT NULL)
);

CREATE INDEX idx_bundles_start_height ON bundles(start_height);

COMMIT;