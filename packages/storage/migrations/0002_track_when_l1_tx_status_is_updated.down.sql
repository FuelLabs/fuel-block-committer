BEGIN;

-- Drop the 'finalized_at' column if it exists
DO $$ 
BEGIN
    IF EXISTS (
        SELECT 1 
        FROM information_schema.columns 
        WHERE table_name = 'l1_transactions' 
        AND column_name = 'finalized_at'
    ) THEN
        ALTER TABLE l1_transactions
        DROP COLUMN finalized_at;
    END IF;
END $$;

COMMIT;
