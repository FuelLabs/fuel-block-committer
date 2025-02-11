BEGIN;

ALTER TABLE l1_blob_transaction RENAME TO da_submission;

ALTER TABLE da_submission
    ADD COLUMN details JSONB NOT NULL DEFAULT '{}'::jsonb;

UPDATE da_submission
SET details = jsonb_build_object(
    'nonce', nonce,
    'max_fee', max_fee,
    'priority_fee', priority_fee,
    'blob_fee', blob_fee
)
WHERE details = '{}'::jsonb;

ALTER TABLE da_submission
    DROP COLUMN nonce,
    DROP COLUMN max_fee,
    DROP COLUMN priority_fee,
    DROP COLUMN blob_fee;

ALTER TABLE l1_transaction_fragments RENAME COLUMN transaction_id TO da_submission_id;
ALTER TABLE l1_transaction_fragments RENAME TO da_submission_fragments;

COMMIT;
