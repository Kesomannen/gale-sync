use axum::extract::FromRequestParts;
use serde::{Deserialize, Serialize};

use crate::prelude::*;

pub mod token;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    // don't expose the id
    #[serde(skip)]
    pub id: i32,
    pub discord_id: String,
    pub name: String,
    pub display_name: String,
    pub avatar: String,
}

/// Extractor to verify and extract the user from the provided token.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = parts
            .headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| AppError::unauthorized("Authorization header is missing."))?;

        let token = auth.strip_prefix("Bearer ").ok_or_else(|| {
            AppError::bad_request("Authorization header must use the Bearer scheme.")
        })?;

        let claims = token::verify(token, state)?;

        Ok(AuthUser(claims.user.into()))
    }
}
