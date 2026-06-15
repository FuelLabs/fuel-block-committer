BEGIN;

-- `total_unbundled_blocks` / `stream_unbundled_blocks` test whether a block height is
-- covered by any bundle's [start_height, end_height] range. A btree on
-- (start_height, end_height) cannot serve range containment, so the planner scanned
-- `bundles` once per candidate block (~20s on mainnet). A GiST index on the int8range
-- expression serves the `@>` containment predicate as an index probe.
--
-- The index expression MUST match the query's: int8range(start_height, end_height, '[]')
-- ('[]' = inclusive both ends, matching the original BETWEEN semantics).
--
-- Measured on mainnet: ~20s -> ~1.1s for the unbundled-blocks count. (A MAX(end_height)
-- watermark was ~5ms but silently omits in-window bundle gaps -- which occur on lookback
-- config changes, see `can_handle_bundled_blocks_appearing_after_unbundled_ones` -- so
-- range containment is used instead to stay correct.)

CREATE INDEX IF NOT EXISTS idx_bundles_range
    ON bundles USING gist (int8range(start_height, end_height, '[]'));

COMMIT;
