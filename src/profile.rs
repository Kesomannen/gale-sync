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
    auth::{self, AuthUser},
    AppError, AppResult, AppState,
};

const SIZE_LIMIT: usize = 1024 * 1024;

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
    community: Option<String>,
    mods: Vec<ProfileMod>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProfileResponse {
    id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const BUCKET_NAME: &str = "gale-sync";

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
    Path(id): Path<Uuid>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(id, &user, &state).await?;

    let profile = upload_profile(id, &user, body, &state).await?;

    Ok(Json(profile))
}

async fn delete_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    check_permission(id, &user, &state).await?;

    let mut tx = state.db.begin().await?;

    sqlx::query!("DELETE FROM profiles WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;

    let res = state
        .s3
        .delete_object()
        .key(s3_key(id))
        .bucket(BUCKET_NAME)
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
        .key(s3_key(profile.id))
        .bucket(BUCKET_NAME)
        .content_encoding("application/zip")
        .acl(ObjectCannedAcl::PublicRead)
        .body(body.into())
        .send()
        .await;

    commit_s3_tx(tx, res, || profile).await
}

fn read_manifest(archive: impl Read + Seek) -> AppResult<ProfileManifest> {
    let mut zip = ZipArchive::new(archive)
        .map_err(|err| AppError::bad_request(format!("malformed zip archive: {err}")))?;

    let manifest = zip
        .by_name("export.r2x")
        .map_err(|_| AppError::bad_request("export.r2x file is missing"))?;

    let manifest: ProfileManifest = serde_yml::from_reader(manifest)
        .map_err(|err| AppError::bad_request(format!("error parsing export.r2x: {err}")))?;

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
        Err(AppError::Forbidden)
    }
}

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

async fn download_profile(Path(id): Path<Uuid>) -> AppResult<Redirect> {
    let url = format!(
        "https://{}.fra1.cdn.digitaloceanspaces.com/{}",
        BUCKET_NAME,
        s3_key(id)
    );

    Ok(Redirect::to(&url))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileMetadata {
    id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    owner: ProfileOwner,
    manifest: ProfileManifest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileOwner {
    name: String,
    display_name: String,
    avatar: String,
    discord_id: i64,
}

async fn get_profile_metadata(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ProfileMetadata>> {
    let profile = sqlx::query!(
        r#"SELECT
            p.id,
            p.name,
            p.community,
            p.mods AS "mods: sqlx::types::Json<Vec<ProfileMod>>",
            p.created_at,
            p.updated_at,
            u.name AS "owner_name",
            u.display_name AS "owner_display_name",
            u.avatar,
            u.discord_id
        FROM profiles p
        JOIN users u ON u.id = p.owner_id
        WHERE p.id = $1"#,
        id
    )
    .map(|record| ProfileMetadata {
        id,
        created_at: record.created_at,
        updated_at: record.updated_at,
        owner: ProfileOwner {
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
