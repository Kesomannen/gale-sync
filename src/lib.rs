use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;

mod auth;
mod error;
mod profile;
mod routes;
mod short_uuid;
pub mod storage;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub storage: storage::Client,
    pub http: reqwest::Client,
    pub discord_client_id: Arc<str>,
    pub discord_client_secret: Arc<str>,
    pub jwt_secret: Arc<str>,
}

pub fn routes(state: AppState) -> Router {
    Router::new()
        .nest("/auth", routes::auth::routes())
        .nest("/profile", routes::profile::routes())
        .nest("/user", routes::user::routes())
        .nest("/desktop", routes::desktop::router())
        .with_state(state)
}

mod prelude {
    pub use super::{
        error::{AppError, AppResult},
        AppState,
    };
}
