ALTER TABLE profiles
ADD COLUMN short_id VARCHAR(22);

UPDATE profiles
SET
    short_id = (
        replace (
            replace (
                trim(
                    trailing '='
                    from
                        (encode (uuid_send (id), 'base64'))
                ),
                '/',
                '_'
            ),
            '+',
            '-'
        )
    );

ALTER TABLE profiles
ALTER COLUMN short_id
SET
    NOT NULL;

ALTER TABLE profiles ADD CONSTRAINT short_id_unique UNIQUE (short_id);

CREATE INDEX idx_profiles_short_id ON profiles (short_id);