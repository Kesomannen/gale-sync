use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jwt::{SignWithKey, VerifyWithKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::prelude::*;

use super::User;

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    #[serde(rename = "exp")]
    pub expiration: i64,

    #[serde(flatten)]
    pub user: JwtUser,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JwtUser {
    #[serde(rename = "sub")]
    id: i32,
    discord_id: String,
    name: String,
    display_name: String,
    avatar: String,
}

impl From<JwtUser> for User {
    fn from(value: JwtUser) -> Self {
        User {
            id: value.id,
            discord_id: value.discord_id,
            name: value.name,
            display_name: value.display_name,
            avatar: value.avatar,
        }
    }
}

impl From<User> for JwtUser {
    fn from(value: User) -> Self {
        JwtUser {
            id: value.id,
            discord_id: value.discord_id,
            name: value.name,
            display_name: value.display_name,
            avatar: value.avatar,
        }
    }
}

fn hmac_key(state: &AppState) -> anyhow::Result<Hmac<Sha256>> {
    Hmac::new_from_slice(state.jwt_secret.as_bytes()).context("failed to create encryption key")
}

pub fn create(user: JwtUser, state: &AppState) -> AppResult<String> {
    const EXPIRATION_TIME: Duration = Duration::from_secs(30 * 60); // 30 minutes

    let key = hmac_key(state)?;
    let claims = JwtClaims {
        user,
        expiration: (Utc::now() + EXPIRATION_TIME).timestamp(),
    };

    let jwt = claims.sign_with_key(&key).context("failed to sign JWT")?;

    Ok(jwt)
}

pub fn verify(token: &str, state: &AppState) -> AppResult<JwtClaims> {
    let key = hmac_key(state)?;
    let claims: JwtClaims = token
        .verify_with_key(&key)
        .map_err(|_| AppError::unauthorized("Token is invalid."))?;

    let expiration = DateTime::from_timestamp(claims.expiration, 0)
        .ok_or_else(|| AppError::unauthorized("Token expiration time is invalid."))?;

    if Utc::now() < expiration {
        Ok(claims)
    } else {
        Err(AppError::unauthorized("Token is expired."))
    }
}
