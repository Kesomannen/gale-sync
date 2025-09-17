use std::io::{Cursor, Read, Seek};

use anyhow::anyhow;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    response::Redirect,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use http::StatusCode;
use rand::Rng;
use serde::Serialize;
use zip::ZipArchive;

use crate::{
    auth::{self, AuthUser},
    prelude::*,
    profile::{self, ProfileId, ProfileManifest, ProfileMetadata},
};

const SIZE_LIMIT: usize = 2 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            post(create_profile).layer(DefaultBodyLimit::max(SIZE_LIMIT)),
        )
        .route(
            "/{id}",
            put(update_profile).layer(DefaultBodyLimit::max(SIZE_LIMIT)),
        )
        .route("/{id}", get(download_profile).delete(delete_profile))
        .route("/{id}/meta", get(get_profile_metadata))
}

async fn create_profile(
    AuthUser(user): AuthUser,
    State(mut state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<CreateProfileResponse>)> {
    let id = generate_id(&state).await?;

    let profile = upload_and_notify(id, &user, body, &mut state).await?;

    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update_profile(
    AuthUser(user): AuthUser,
    State(mut state): State<AppState>,
    Path(id): Path<ProfileId>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(&id, &user, &state).await?;

    let profile = upload_and_notify(id, &user, body, &mut state).await?;

    Ok(Json(profile))
}

async fn delete_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<ProfileId>,
) -> AppResult<StatusCode> {
    check_permission(&id, &user, &state).await?;

    let mut tx = state.db.begin().await?;

    let _ = sqlx::query!(
        "DELETE FROM profiles WHERE short_id = $1 RETURNING updated_at",
        &*id.as_str()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    state
        .sockets
        .notify_profile_deleted(state.redis.clone(), &id);

    Ok(StatusCode::NO_CONTENT)
}

async fn check_permission(
    profile_id: &ProfileId,
    user: &auth::User,
    state: &AppState,
) -> Result<(), AppError> {
    let profile = sqlx::query!(
        "SELECT owner_id FROM profiles WHERE short_id = $1",
        &*profile_id.as_str()
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    if profile.owner_id == user.id {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "User is not the owner of this profile.",
        ))
    }
}

async fn download_profile(
    Path(id): Path<ProfileId>,
    State(state): State<AppState>,
) -> AppResult<Redirect> {
    let profile = sqlx::query!(
        "UPDATE profiles
            SET downloads = downloads + 1
        WHERE short_id = $1
        RETURNING 
            updated_at,
            code",
        &*id.as_str()
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let code = profile
        .code
        .map(|uuid| uuid.to_string())
        .ok_or_else(|| AppError::Other(anyhow!("profile has no thunderstore code")))?;

    let url = format!("https://thunderstore.io/api/experimental/legacyprofile/get/{code}/");

    Ok(Redirect::to(&url))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProfileResponse {
    #[serde(rename = "id")]
    short_id: ProfileId,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn upload_and_notify(
    id: ProfileId,
    user: &auth::User,
    body: Bytes,
    state: &mut AppState,
) -> AppResult<CreateProfileResponse> {
    let cursor = Cursor::new(body.clone());
    // reading the zip file could be intensive
    let manifest = tokio::task::spawn_blocking(|| read_manifest(cursor))
        .await
        .map_err(|err| anyhow!(err))??;

    let mods_json = serde_json::to_value(&manifest.mods)
        .map_err(|err| anyhow!("failed to serialize mods: {err}"))?;

    let key = profile::upload(state, body).await?;

    let profile = sqlx::query_as!(
        CreateProfileResponse,
        r#"INSERT INTO profiles (short_id, owner_id, name, community, mods, code)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT(short_id)
        DO UPDATE SET
            name = EXCLUDED.name,
            mods = EXCLUDED.mods,
            code = EXCLUDED.code,
            updated_at = NOW()
        RETURNING
            short_id AS "short_id: ProfileId", 
            created_at,
            updated_at"#,
        &*id.as_str(),
        user.id,
        manifest.profile_name,
        manifest.community,
        mods_json,
        key
    )
    .fetch_one(&state.db)
    .await?;

    state.sockets.notify_profile_updated(
        state.redis.clone(),
        &ProfileMetadata {
            short_id: id,
            created_at: profile.created_at,
            updated_at: profile.updated_at,
            owner: user.clone(),
            manifest,
        },
    );

    Ok(profile)
}

fn read_manifest(input: impl Read + Seek) -> AppResult<ProfileManifest> {
    let mut input_zip = ZipArchive::new(input)
        .map_err(|err| AppError::bad_request(format!("Invalid ZIP archive: {err}")))?;

    let manifest = input_zip
        .by_name("export.r2x")
        .map_err(|_| AppError::bad_request("Invalid ZIP archive: export.r2x file is missing"))?;

    let manifest: ProfileManifest = serde_yml::from_reader(manifest)
        .map_err(|err| AppError::bad_request(format!("Error parsing export.r2x: {err}")))?;

    Ok(manifest)
}

async fn get_profile_metadata(
    State(state): State<AppState>,
    Path(id): Path<ProfileId>,
) -> AppResult<Json<ProfileMetadata>> {
    let profile = crate::profile::get(&state, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(profile))
}

async fn generate_id(state: &AppState) -> AppResult<ProfileId> {
    loop {
        let id: String = rand::rng()
            .sample_iter(rand::distr::Alphanumeric)
            .take(6)
            .map(char::from)
            .map(|c| c.to_ascii_uppercase())
            .collect();

        if rustrict::CensorStr::is_inappropriate(&*id) {
            continue;
        }

        let exists = sqlx::query!(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE short_id = $1)",
            &id
        )
        .fetch_one(&state.db)
        .await?
        .exists
        .unwrap_or(true);

        if exists {
            continue;
        }

        return Ok(ProfileId::Short(id));
    }
}
