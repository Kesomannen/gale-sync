use std::{future::Future, io::{Cursor, Read, Seek}};

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
use serde::Serialize;
use sqlx::Postgres;
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    auth::{self, AuthUser, User},
    prelude::*,
    profile::{ProfileManifest, ProfileMod},
    short_uuid::ShortUuid,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProfileResponse {
    id: ShortUuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn create_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<CreateProfileResponse>)> {
    let profile = upload_profile(Uuid::new_v4(), &user, body, true, &state).await?;

    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<ShortUuid>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(id.0, &user, &state).await?;

    let profile = upload_profile(id.0, &user, body, false, &state).await?;

    Ok(Json(profile))
}

async fn delete_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<ShortUuid>,
) -> AppResult<StatusCode> {
    check_permission(id.0, &user, &state).await?;

    let mut tx = state.db.begin().await?;

    let _ = sqlx::query!(
        "DELETE FROM profiles WHERE id = $1 RETURNING updated_at",
        id.0
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    commit_s3_tx(tx, state.storage.delete(s3_key(id.0))).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn upload_profile(
    id: Uuid,
    user: &auth::User,
    body: Bytes,
    post: bool,
    state: &AppState,
) -> Result<CreateProfileResponse, AppError> {
    let cursor = Cursor::new(body.clone());
    // reading the zip file could be intensive
    let manifest = tokio::task::spawn_blocking(|| read_manifest(cursor))
        .await
        .map_err(|err| anyhow!(err))??;

    let mods_json = serde_json::to_value(manifest.mods)
        .map_err(|err| anyhow!("failed to serialize mods: {err}"))?;

    let mut tx = state.db.begin().await?;

    let profile = sqlx::query_as!(
        CreateProfileResponse,
        "INSERT INTO profiles (id, owner_id, name, community, mods)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT(id)
        DO UPDATE SET
            name = EXCLUDED.name,
            mods = EXCLUDED.mods,
            updated_at = NOW()
        RETURNING id, created_at, updated_at",
        id,
        user.id,
        manifest.profile_name,
        manifest.community,
        mods_json
    )
    .fetch_one(&mut *tx)
    .await?;

    commit_s3_tx(tx, state.storage.upload(s3_key(profile.id.0), body, post)).await?;

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

    // maybe re-encode the zip if its not compressed?
    // also check for unusual file extensions

    /*
    let mut output = Vec::new();
    let mut output_zip = ZipWriter::new(Cursor::new(&mut output));

    let opts = FileOptions::<'_, ()>::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    for i in 0..input_zip.len() {
        let mut file = input_zip.by_index(i).context("failed to read file")?;

        output_zip
            .start_file(file.name(), opts)
            .context("failed to start file")?;

        std::io::copy(&mut file, &mut output_zip).context("failed to re-encode file")?;
    }
    */

    Ok(manifest)
}

async fn check_permission(profile_id: Uuid, user: &auth::User, state: &AppState) -> Result<(), AppError> {
    let profile = sqlx::query!("SELECT owner_id FROM profiles WHERE id = $1", profile_id)
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

/// This makes sure an s3 operation and a database transaction
/// happens atomically (by rolling back the db if s3 failed).
///
/// It's not perfect, since if the final database commit fails
/// s3 will still go through.
async fn commit_s3_tx<T, Fut>(
    tx: sqlx::Transaction<'_, Postgres>,
    s3: Fut,
) -> AppResult<T>
where 
    Fut: Future<Output = AppResult<T>> 
{
    match s3.await {
        Ok(res) => {
            tx.commit().await?;
            Ok(res)
        }
        Err(err) => {
            tx.rollback().await?;
            Err(anyhow!("Storage error: {err}").into())
        }
    }
}

async fn download_profile(
    Path(id): Path<ShortUuid>,
    State(state): State<AppState>,
) -> AppResult<Redirect> {
    let profile = sqlx::query!(
        "UPDATE profiles
            SET downloads = downloads + 1
        WHERE id = $1
        RETURNING updated_at",
        id.0
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Include a versioned query param to make sure we aren't getting
    // an already cached old version
    let url = state.storage.object_url(format!(
        "{}?v={}",
        s3_key(id.0),
        profile.updated_at.timestamp()
    ));

    Ok(Redirect::to(&url))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileMetadata {
    id: ShortUuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    owner: User,
    manifest: ProfileManifest,
}

async fn get_profile_metadata(
    State(state): State<AppState>,
    Path(id): Path<ShortUuid>,
) -> AppResult<Json<ProfileMetadata>> {
    let profile = sqlx::query!(
        r#"SELECT
            p.id,
            p.name,
            p.community,
            p.mods AS "mods: sqlx::types::Json<Vec<ProfileMod>>",
            p.created_at,
            p.updated_at,
            u.id AS "owner_id",
            u.name AS "owner_name",
            u.display_name AS "owner_display_name",
            u.avatar,
            u.discord_id
        FROM profiles p
        JOIN users u ON u.id = p.owner_id
        WHERE p.id = $1"#,
        id.0
    )
    .map(|record| ProfileMetadata {
        id,
        created_at: record.created_at,
        updated_at: record.updated_at,
        owner: User {
            id: record.owner_id,
            name: record.owner_name,
            display_name: record.owner_display_name,
            avatar: record.avatar,
            discord_id: record.discord_id,
        },
        manifest: ProfileManifest {
            profile_name: record.name,
            community: record.community,
            mods: record.mods.0,
        },
    })
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(profile))
}

fn s3_key(id: Uuid) -> String {
    format!("profile/{id}.zip")
}
