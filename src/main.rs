use std::{env, sync::Arc, time::Instant};

use anyhow::{anyhow, Context};
use axum::Router;
use dotenvy::dotenv;
use gale_sync::AppState;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{debug, info, Level};

const DEFAULT_PORT: u16 = 8080;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start = Instant::now();

    dotenv().ok();

    let log_level = env_var("LOG_LEVEL")
        .map(|str| {
            str.parse()
                .expect("LOG_LEVEL variable is not a valid log level")
        })
        .unwrap_or(Level::INFO);

    tracing_subscriber::fmt()
        .compact()
        .with_max_level(log_level)
        .init();

    info!(
        "gale-sync v{} running on {}-{}",
        env!("CARGO_PKG_VERSION"),
        env::consts::OS,
        env::consts::ARCH
    );

    let (redis_tx, redis_rx) = mpsc::unbounded_channel();

    let (db, redis) = tokio::try_join!(setup_db(), setup_redis(redis_tx))?;

    let sockets = gale_sync::socket::State::new(redis_rx);

    let http = reqwest::Client::new();

    let storage = gale_sync::storage::Client::new(
        env_var_arc("STORAGE_BUCKET_NAME")?,
        env_var_arc("SUPABASE_API_KEY")?,
        format!("{}/storage/v1", env_var("SUPABASE_URL")?).into(),
        http.clone(),
    );

    let state = AppState {
        db,
        http,
        storage,
        discord_client_id: env_var_arc("DISCORD_CLIENT_ID")?,
        discord_client_secret: env_var_arc("DISCORD_CLIENT_SECRET")?,
        jwt_secret: env_var_arc("JWT_SECRET")?,
        sockets,
        redis,
    };

    let app = Router::new()
        .nest("/api", gale_sync::routes(state))
        .fallback_service(ServeDir::new("public"))
        .layer(TraceLayer::new_for_http());

    let port = env_var("PORT")
        .map(|str| str.parse().expect("PORT variable is not a valid integer"))
        .unwrap_or(DEFAULT_PORT);

    info!("listening on port {port}");

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;

    info!("ready to serve in {:?}", start.elapsed());

    axum::serve(listener, app).await?;
    Ok(())
}

async fn setup_redis(
    sender: impl redis::aio::AsyncPushSender,
) -> anyhow::Result<redis::aio::MultiplexedConnection> {
    let url = env_var("REDIS_URL")?;

    debug!("connecting to redis at {url}");

    let mut redis = redis::Client::open(url)?
        .get_multiplexed_async_connection_with_config(
            &redis::AsyncConnectionConfig::new().set_push_sender(sender),
        )
        .await
        .context("failed to establish redis connection")?;

    redis.psubscribe("profile-update:*").await?;
    redis.psubscribe("profile-delete:*").await?;

    Ok(redis)
}

async fn setup_db() -> anyhow::Result<PgPool> {
    let db_url = env_var("DATABASE_URL")?;

    debug!("connecting to database at {db_url}");

    let db = PgPool::connect(&db_url).await?;

    //sqlx::migrate!().run(&db).await?;
    Ok(db)
}

fn env_var_arc(name: &str) -> anyhow::Result<Arc<str>> {
    env_var(name).map(Into::into)
}

fn env_var(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow!("{name} is not set"))
}
