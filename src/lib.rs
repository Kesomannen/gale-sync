use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;

mod auth;
mod error;
mod profile;
mod redirect;
mod routes;
mod short_uuid;
pub mod socket;
pub mod storage;

type RedisConn = redis::aio::MultiplexedConnection;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub storage: storage::Client,
    pub http: reqwest::Client,
    pub discord_client_id: Arc<str>,
    pub discord_client_secret: Arc<str>,
    pub jwt_secret: Arc<str>,
    pub sockets: socket::State,
    pub redis: RedisConn,
}

pub fn routes(state: AppState) -> Router {
    Router::new()
        .nest("/auth", routes::auth::routes())
        .nest("/profile", routes::profile::routes())
        .nest("/user", routes::user::routes())
        .nest("/desktop", routes::desktop::routes())
        .nest("/socket", routes::socket::routes())
        .with_state(state)
}

mod prelude {
    pub use super::{
        error::{AppError, AppResult},
        AppState,
    };
}
