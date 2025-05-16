use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;

mod auth;
mod error;
mod profile;
mod short_uuid;

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: PgPool,
    pub s3: aws_sdk_s3::Client,
    pub http: reqwest::Client,
    pub discord_client_id: Arc<str>,
    pub discord_client_secret: Arc<str>,
    pub jwt_secret: Arc<str>,
    pub cdn_domain: Arc<str>,
}

pub fn routes(state: AppState) -> Router {
    Router::new()
        .nest("/auth", auth::routes())
        .nest("/profile", profile::routes())
        .with_state(state)
}

mod prelude {
    pub use super::{
        error::{AppError, AppResult},
        AppState,
    };
}
