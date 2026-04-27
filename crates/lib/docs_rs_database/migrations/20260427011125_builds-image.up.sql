ALTER TABLE builds ADD COLUMN build_image TEXT;
CREATE INDEX builds_build_image_idx ON builds USING btree (build_image) ;
