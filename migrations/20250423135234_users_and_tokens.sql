CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    discord_id BIGINT NOT NULL UNIQUE
);
