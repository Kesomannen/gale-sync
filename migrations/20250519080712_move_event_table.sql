CREATE TYPE event_kind AS ENUM ('app_start');

CREATE TABLE IF NOT EXISTS event (
  id SERIAL PRIMARY KEY,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  kind event_kind NOT NULL,
  user_id UUID NOT NULL
);
