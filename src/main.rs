use std::sync::Arc;

use anyhow::anyhow;
use aws_config::Region;
use dotenvy::dotenv;
use gale_sync::AppState;
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use tracing::{info, Level};

const DEFAULT_PORT: u16 = 8800;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let log_level = std::env::var("LOG_LEVEL")
        .map(|str| {
            str.parse()
                .expect("LOG_LEVEL variable is not a valid log level")
        })
        .unwrap_or(Level::INFO);

    tracing_subscriber::fmt()
        .compact()
        .with_max_level(log_level)
        .init();

    let db_url = env_var("DATABASE_URL")?;
    let db = PgPool::connect(&db_url).await?;

    sqlx::migrate!().run(&db).await?;

    let config = aws_config::from_env()
        .region(Region::new("fra1"))
        .endpoint_url("https://fra1.digitaloceanspaces.com")
        .load()
        .await;

    let s3 = aws_sdk_s3::Client::new(&config);

    let state = AppState {
        db,
        s3,
        http: reqwest::Client::new(),
        discord_client_id: env_var("DISCORD_CLIENT_ID")?,
        discord_client_secret: env_var("DISCORD_CLIENT_SECRET")?,
        jwt_secret: env_var("JWT_SECRET")?,
    };

    let app = gale_sync::routes(state).layer(TraceLayer::new_for_http());

    let port = std::env::var("PORT")
        .map(|str| str.parse().expect("PORT variable is not a valid integer"))
        .unwrap_or(DEFAULT_PORT);

    info!("listening on port {port}");

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;

    axum::serve(listener, app).await?;
    Ok(())
}

fn env_var(name: &str) -> anyhow::Result<Arc<str>> {
    match std::env::var(name) {
        Ok(str) => Ok(str.into()),
        Err(_) => Err(anyhow!("{name} is not set")),
    }
}
