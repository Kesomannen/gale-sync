ALTER TABLE users
DROP COLUMN discriminator,
DROP COLUMN public_flags,
ALTER COLUMN avatar DROP NOT NULL;
