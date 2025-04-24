use std::time::Duration;

use anyhow::Context;
use axum::{
    extract::{FromRequestParts, Query, State},
    response::Redirect,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use http::StatusCode;
use jwt::{SignWithKey, VerifyWithKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use url::Url;
use uuid::Uuid;

use crate::{AppError, AppResult, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(oauth_callback))
        .route("/token", post(grant_token))
        .route("/me", get(me))
}

const DISCORD_API_ENDPOINT: &str = "https://discord.com/api/v10";
const SERVER_REDIRECT_URI: &str = "http://localhost:8800/api/auth/callback";
const CLIENT_REDIRECT_URI: &str = "http://localhost:22942";

async fn login(State(state): State<AppState>) -> AppResult<Redirect> {
    let uuid = Uuid::new_v4().to_string();

    sqlx::query!("INSERT INTO oauth_flow (state) VALUES ($1)", uuid)
        .execute(&state.db)
        .await?;

    let mut url = Url::parse(&format!("{DISCORD_API_ENDPOINT}/oauth2/authorize")).unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &state.discord_client_id)
        .append_pair("scope", "identify")
        .append_pair("redirect_uri", SERVER_REDIRECT_URI)
        .append_pair("state", &uuid);

    Ok(Redirect::to(url.as_str()))
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    state: String,
    code: String,
}

#[derive(Debug, Deserialize)]
struct DiscordTokens {
    token_type: String,
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct DiscordAuthInfo {
    user: DiscordUser,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    avatar: String,
    discriminator: String,
    global_name: String,
    public_flags: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}

async fn oauth_callback(
    Query(query): Query<CallbackQuery>,
    State(state): State<AppState>,
) -> AppResult<Redirect> {
    complete_oauth_flow(&query.state, &state).await?;

    let tokens = request_token_and_create_jwt(
        DiscordTokenRequest::AuthorizationCode {
            code: &query.code,
            redirect_uri: SERVER_REDIRECT_URI,
        },
        &state,
    )
    .await?;

    let mut client_redirect = Url::parse(CLIENT_REDIRECT_URI).unwrap();
    client_redirect
        .query_pairs_mut()
        .append_pair("access_token", &tokens.access_token)
        .append_pair("refresh_token", &tokens.refresh_token);

    Ok(Redirect::to(client_redirect.as_str()))
}

async fn me(AuthUser(user): AuthUser) -> AppResult<Json<User>> {
    Ok(Json(user))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantTokenRequest {
    refresh_token: String,
}

async fn grant_token(
    State(state): State<AppState>,
    Json(req): Json<GrantTokenRequest>,
) -> AppResult<Json<TokenResponse>> {
    request_token_and_create_jwt(
        DiscordTokenRequest::RefreshToken {
            refresh_token: &req.refresh_token,
        },
        &state,
    )
    .await
    .map(Json)
}

async fn request_token_and_create_jwt(
    req: DiscordTokenRequest<'_>,
    state: &AppState,
) -> AppResult<TokenResponse> {
    let tokens = get_discord_token(req, &state).await?;
    let info = get_discord_auth_info(&tokens.access_token, &state).await?;

    let user = upsert_discord_user(info.user, &state).await?;
    let jwt = create_jwt(user.into(), &state)?;

    Ok(TokenResponse {
        access_token: jwt,
        refresh_token: tokens.refresh_token,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "grant_type")]
enum DiscordTokenRequest<'a> {
    AuthorizationCode {
        code: &'a str,
        redirect_uri: &'a str,
    },
    RefreshToken {
        refresh_token: &'a str,
    },
}

async fn get_discord_token(
    req: DiscordTokenRequest<'_>,
    state: &AppState,
) -> AppResult<DiscordTokens> {
    let res = state
        .http
        .post(format!("{DISCORD_API_ENDPOINT}/oauth2/token"))
        .form(&req)
        .basic_auth(&state.discord_client_id, Some(&state.discord_client_secret))
        .send()
        .await?
        .error_for_status();

    match res {
        Ok(res) => {
            let tokens: DiscordTokens = res.json().await?;
            Ok(tokens)
        }
        Err(err) if err.status() == Some(StatusCode::BAD_REQUEST) => {
            Err(AppError::bad_request("Invalid refresh token."))
        }
        Err(err) => Err(err.into()),
    }
}

async fn get_discord_auth_info(access_token: &str, state: &AppState) -> AppResult<DiscordAuthInfo> {
    let info: DiscordAuthInfo = state
        .http
        .get(format!("{DISCORD_API_ENDPOINT}/oauth2/@me"))
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(info)
}

async fn upsert_discord_user(user: DiscordUser, state: &AppState) -> AppResult<User> {
    let discord_id: i64 = user.id.parse().context("invalid discord id")?;

    let user = sqlx::query_as!(
        User,
        "INSERT INTO users (name, display_name, discord_id, avatar, discriminator, public_flags)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT(discord_id)
        DO UPDATE SET
            name = EXCLUDED.name,
            display_name = EXCLUDED.display_name,
            avatar = EXCLUDED.avatar,
            discriminator = EXCLUDED.discriminator,
            public_flags = EXCLUDED.public_flags
        RETURNING id, name, display_name, discord_id, avatar",
        user.username,
        user.global_name,
        discord_id,
        user.avatar,
        user.discriminator,
        user.public_flags as i32,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(user)
}

async fn complete_oauth_flow(query_state: &str, state: &AppState) -> Result<(), AppError> {
    let session = sqlx::query!(
        "SELECT completed FROM oauth_flow WHERE state = $1",
        query_state
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Forbidden)?;

    if session.completed {
        return Err(AppError::Forbidden);
    }

    sqlx::query!(
        "UPDATE oauth_flow SET completed = TRUE WHERE state = $1",
        query_state
    )
    .execute(&state.db)
    .await?;

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(skip)]
    pub id: i32,
    pub discord_id: i64,
    pub name: String,
    pub display_name: String,
    pub avatar: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    #[serde(rename = "exp")]
    expiration: i64,

    #[serde(flatten)]
    user: JwtUser,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JwtUser {
    #[serde(rename = "sub")]
    id: i32,
    discord_id: i64,
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

fn create_jwt(user: JwtUser, state: &AppState) -> AppResult<String> {
    const EXPIRATION_TIME: Duration = Duration::from_secs(30 * 60);

    let key = hmac_key(state)?;
    let claims = JwtClaims {
        user,
        expiration: (Utc::now() + EXPIRATION_TIME).timestamp(),
    };

    let jwt = claims.sign_with_key(&key).context("failed to sign JWT")?;

    Ok(jwt)
}

fn verify_jwt(token: &str, state: &AppState) -> AppResult<JwtClaims> {
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

        let claims = verify_jwt(token, state)?;

        Ok(AuthUser(claims.user.into()))
    }
}
