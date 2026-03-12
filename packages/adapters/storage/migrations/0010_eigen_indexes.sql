BEGIN;

CREATE INDEX idx_eigen_submission_fragments_id_desc ON eigen_submission_fragments (id DESC);
CREATE INDEX idx_eigen_submission_status ON eigen_submission (status);
CREATE INDEX idx_l1_fragments_bundle_id ON l1_fragments (bundle_id);
CREATE INDEX idx_eigen_submission_status_id ON eigen_submission (status, id);

COMMIT;
