# Hosting

## Environment Variables

| **Name**                | **Description**                              | **Default** |
|-------------------------|----------------------------------------------|-------------|
| `DATABASE_URL`          | Postgres connection URL                      | *           |
| `DISCORD_CLIENT_ID`     | Client ID of Discord OAuth app               | *           |
| `DISCORD_CLIENT_SECRET` | Client secret of Discord OAuth app           | *           |
| `JWT_SECRET`            | Secret key for JWT signing                   | *           |
| `AWS_ACCESS_KEY_ID`     | Access key ID for S3                         | *           |
| `AWS_SECRET_ACCESS_KEY` | Access secret key for S3                     | *           |
| `S3_REGION`             | Region for S3                                | *           |
| `S3_ENDPOINT`           | Full URL of the S3 endpoint                  | *           |
| `CDN_DOMAIN`            | Domain name of the CDN to use in front of S3 | *           |
| `LOG_LEVEL`             | Max log level                                | `INFO`      |
| `PORT`                  | Port to listen at                            | 8080        |
