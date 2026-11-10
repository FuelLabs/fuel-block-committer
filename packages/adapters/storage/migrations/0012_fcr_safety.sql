-- Explicitly initialized per writer; absence is a closed safety gate.
CREATE TABLE fcr_safety (writer TEXT PRIMARY KEY, state JSONB NOT NULL);
