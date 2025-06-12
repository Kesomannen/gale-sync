# Hosting

The backend uses Supabase Postgres and Storage.

## Environment Variables

| **Name**                | **Description**                            | **Default** |
| ----------------------- | ------------------------------------------ | ----------- |
| `DATABASE_URL`          | Postgres connection URL                    | \*          |
| `DISCORD_CLIENT_ID`     | Client ID of Discord OAuth app             | \*          |
| `DISCORD_CLIENT_SECRET` | Client secret of Discord OAuth app         | \*          |
| `JWT_SECRET`            | Secret key for JWT signing                 | \*          |
| `SUPABASE_URL`          | URL of the Supabase project                | \*          |
| `SUPABASE_API_KEY`      | Service role API key for Supabase          | \*          |
| `STORAGE_BUCKET_NAME`   | Name of the Supabase storage bucket to use | \*          |
| `LOG_LEVEL`             | Max log level                              | `INFO`      |
| `PORT`                  | Port to listen at                          | 8080        |
