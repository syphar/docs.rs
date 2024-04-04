ALTER TABLE builds 
    ADD COLUMN documentation_size BIGINT,
    ADD COLUMN documentation_size_compressed BIGINT,
    ADD COLUMN source_size BIGINT,
    ADD COLUMN source_size_compressed BIGINT
;

