BEGIN;
    
ALTER TABLE fuel_blocks DROP CONSTRAINT fuel_blocks_pkey;

ALTER TABLE fuel_blocks DROP COLUMN hash;

ALTER TABLE fuel_blocks ADD PRIMARY KEY (height);

COMMIT;
