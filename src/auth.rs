use std::time::Duration;

use anyhow::Context;
use axum::{
    extract::{FromRequestParts, Query, State},
    response::{Html, Redirect},
    routing::{get, post},
    Json, Router,
};
use axum_extra::extract::{
    cookie::{Cookie, SameSite},
    CookieJar,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use http::StatusCode;
use jwt::{SignWithKey, VerifyWithKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::warn;
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

#[cfg(debug_assertions)]
const SERVER_REDIRECT_URI: &str = "http://localhost:8080/api/auth/callback";

#[cfg(not(debug_assertions))]
const SERVER_REDIRECT_URI: &str = "http://gale.kesomannen.com/api/auth/callback";

async fn login(
    State(state): State<AppState>,
    mut cookies: CookieJar,
) -> AppResult<(CookieJar, Redirect)> {
    let oauth_state = Uuid::new_v4().to_string();

    let mut url = Url::parse(&format!("{DISCORD_API_ENDPOINT}/oauth2/authorize")).unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &state.discord_client_id)
        .append_pair("scope", "identify")
        .append_pair("redirect_uri", SERVER_REDIRECT_URI)
        .append_pair("state", &oauth_state);

    let mut cookie = Cookie::new("state", oauth_state);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(true);
    cookie.set_http_only(true);
    cookies = cookies.add(cookie);

    Ok((cookies, Redirect::to(url.as_str())))
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    state: String,
    code: String,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
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
    cookies: CookieJar,
) -> AppResult<Html<String>> {
    let auth_state = cookies
        .get("state")
        .ok_or(AppError::bad_request("OAuth state cookie is missing."))?
        .value();

    if auth_state != query.state {
        return Err(AppError::bad_request("OAuth state parameter is invalid."));
    }

    let tokens = request_token_and_create_jwt(
        DiscordTokenRequest::AuthorizationCode {
            code: &query.code,
            redirect_uri: SERVER_REDIRECT_URI,
        },
        &state,
    )
    .await?;

    let html = include_str!("../assets/redirect.html")
        .replace("%access_token%", &tokens.access_token)
        .replace("%refresh_token%", &tokens.refresh_token);

    Ok(Html(html))
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
    let tokens = get_discord_token(req, state).await?;
    let info = get_discord_auth_info(&tokens.access_token, state)
        .await
        .context("error fetching discord auth info")?;

    let user = upsert_discord_user(info.user, state).await?;
    let jwt = create_jwt(user.into(), state)?;

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
        .await
        .context("error sending discord token request")?
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
    let is_test_user = sqlx::query!(
        "SELECT EXISTS(SELECT 1 FROM test_users WHERE discord_id = $1)",
        user.id
    )
    .fetch_one(&state.db)
    .await?
    .exists
    .unwrap_or(false);

    if !is_test_user {
        warn!(
            "user {} tried to log in but wasn't whitelisted!",
            user.global_name
        );
        return Err(AppError::forbidden("Profile sync is currently only available to test users. Request beta access on Discord or come back later!"));
    }

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
        user.id,
        user.avatar,
        user.discriminator,
        user.public_flags as i32,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(user)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(skip)]
    pub id: i32,
    pub discord_id: String,
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
