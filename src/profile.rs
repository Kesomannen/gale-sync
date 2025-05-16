use std::io::{Cursor, Read, Seek};

use anyhow::anyhow;
use aws_sdk_s3::types::ObjectCannedAcl;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    response::Redirect,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Postgres;
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    auth::{self, AuthUser, User},
    short_uuid::ShortUuid,
    AppError, AppResult, AppState,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileMod {
    name: String,
    enabled: bool,
    version: ModVersion,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileManifest {
    profile_name: String,
    #[serde(default)]
    community: Option<String>,
    mods: Vec<ProfileMod>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProfileResponse {
    id: ShortUuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const S3_BUCKET_NAME: &str = "gale-sync";

async fn create_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<CreateProfileResponse>)> {
    let profile = upload_profile(Uuid::new_v4(), &user, body, &state).await?;

    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<ShortUuid>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(id.0, &user, &state).await?;

    let profile = upload_profile(id.0, &user, body, &state).await?;

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

    let res = state
        .s3
        .delete_object()
        .key(s3_key(id.0))
        .bucket(S3_BUCKET_NAME)
        .send()
        .await;

    commit_s3_tx(tx, res, || StatusCode::NO_CONTENT).await
}

async fn upload_profile(
    id: Uuid,
    user: &auth::User,
    body: Bytes,
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

    let res = state
        .s3
        .put_object()
        .key(s3_key(profile.id.0))
        .bucket(S3_BUCKET_NAME)
        .content_encoding("application/zip")
        .acl(ObjectCannedAcl::PublicRead)
        .body(body.into())
        .send()
        .await;

    commit_s3_tx(tx, res, || profile).await
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

async fn check_permission(id: Uuid, user: &auth::User, state: &AppState) -> Result<(), AppError> {
    let profile = sqlx::query!("SELECT owner_id FROM profiles WHERE id = $1", id)
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
async fn commit_s3_tx<T, U>(
    tx: sqlx::Transaction<'_, Postgres>,
    s3_res: Result<U, impl std::error::Error>,
    f: impl FnOnce() -> T,
) -> AppResult<T> {
    match s3_res {
        Ok(_) => {
            tx.commit().await?;
            Ok(f())
        }
        Err(err) => {
            tx.rollback().await?;
            Err(anyhow!("S3 error: {err}").into())
        }
    }
}

async fn download_profile(
    Path(id): Path<ShortUuid>,
    State(state): State<AppState>,
) -> AppResult<Redirect> {
    // Maybe move this to a separate task to not block the response?
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

    // Include a version query to make sure we aren't getting
    // an already cached old version
    let url = format!(
        "https://{}.{}/{}?v={}",
        S3_BUCKET_NAME,
        state.cdn_domain,
        s3_key(id.0),
        profile.updated_at.timestamp()
    );

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
