use anyhow::Context;
use axum::{
    extract::{Query, State},
    response::{Html, Redirect},
    routing::{get, post},
    Json, Router,
};
use axum_extra::extract::{
    cookie::{Cookie, SameSite},
    CookieJar,
};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::{
    auth::{self, User},
    prelude::*,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/login", get(login))
        .route("/callback", get(oauth_callback))
        .route("/token", post(grant_token))
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
    avatar: Option<String>,
    global_name: Option<String>,
}

impl DiscordUser {
    fn display_name(&self) -> &str {
        self.global_name.as_ref().unwrap_or(&self.username)
    }
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
    let oauth_state = cookies
        .get("state")
        .ok_or(AppError::bad_request("OAuth state cookie is missing."))?
        .value();

    if oauth_state != query.state {
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

    // This redirect page sends the user to `gale://auth/callback`
    // to let the app receive the tokens.
    let html = include_str!("../../assets/redirect.html")
        .replace("%access_token%", &tokens.access_token)
        .replace("%refresh_token%", &tokens.refresh_token);

    Ok(Html(html))
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
    let jwt = auth::token::create(user.into(), state)?;

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
    let info = state
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
    /*
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
            user.display_name()
        );
        // TODO: nicer redirect since this is shown in browsers
        return Err(AppError::forbidden("Profile sync is currently only available to test users. Request beta access on Discord or come back later!"));
    }
    */

    let user = sqlx::query_as!(
        User,
        "INSERT INTO users (name, display_name, discord_id, avatar)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT(discord_id)
        DO UPDATE SET
            name = EXCLUDED.name,
            display_name = EXCLUDED.display_name,
            avatar = EXCLUDED.avatar
        RETURNING id, name, display_name, discord_id, avatar",
        user.username,
        user.display_name(),
        user.id,
        user.avatar,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(user)
}
