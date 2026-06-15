BEGIN;

-- The "oldest unsubmitted/nonfinalized fragments" anti-joins filter already-submitted
-- fragments via `<link_table>.fragment_id = f.id`. The only indexes on these link
-- tables are the composite UNIQUE (submission_id/transaction_id, fragment_id), whose
-- leading column is the *submission/transaction* id -- so `WHERE fragment_id = $1`
-- cannot seek and seq-scans the link table once per candidate fragment.
--
-- Adding a standalone fragment_id index turns that probe into an index seek.
-- Measured on mainnet for `_oldest_unsubmitted_fragments`: 487ms -> 30ms
-- (the per-fragment seq scan of eigen_submission_fragments was ~162k of ~165k
-- shared buffers touched).

CREATE INDEX IF NOT EXISTS idx_eigen_submission_fragments_fragment_id
    ON eigen_submission_fragments (fragment_id);

CREATE INDEX IF NOT EXISTS idx_l1_transaction_fragments_fragment_id
    ON l1_transaction_fragments (fragment_id);

COMMIT;
