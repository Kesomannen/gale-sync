use axum::{extract::State, routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{auth::AuthUser, prelude::*, short_uuid::ShortUuid};

pub fn routes() -> Router<AppState> {
    Router::new()
        //.route("/{name}", get(get_user))
        .route("/me", get(me))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct User {
    discord_id: String,
    name: String,
    display_name: String,
    avatar: String,
    profiles: Option<Vec<UserProfile>>,
}

#[derive(Debug, Serialize, sqlx::Type)]
#[serde(rename_all = "camelCase")]
struct UserProfile {
    id: ShortUuid,
    name: String,
    community: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn me(AuthUser(user): AuthUser, State(state): State<AppState>) -> AppResult<Json<User>> {
    query_user(user.name, &state).await.map(Json)
}

// this might get added later

/*
async fn get_user(
    Path(name): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<User>> {
    query_user(name, &state).await.map(Json)
}
*/

async fn query_user(name: String, state: &AppState) -> AppResult<User> {
    let user = sqlx::query_as!(
        User,
        r#"SELECT
            u.discord_id,
            u.name,
            u.display_name,
            u.avatar,
            COALESCE(
                ARRAY_AGG ((
                    p.id,
                    p.name,
                    p.community,
                    p.created_at,
                    p.updated_at
                )) FILTER (WHERE p.id IS NOT NULL),
                ARRAY[]::record[]
            ) AS "profiles: Vec<UserProfile>"
        FROM users u
        LEFT JOIN profiles p
            ON p.owner_id = u.id
        WHERE u.name = $1
        GROUP BY
            u.discord_id,
            u.name,
            u.display_name,
            u.avatar"#,
        name
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(user)
}
