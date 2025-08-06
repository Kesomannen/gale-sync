use std::{
    fmt::Display,
    future::Future,
    io::{Cursor, Read, Seek},
};

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
use sqlx::Postgres;
use zip::ZipArchive;

use crate::{
    auth::{self, AuthUser, User},
    prelude::*,
    profile::{ProfileId, ProfileManifest, ProfileMetadata, ProfileMod},
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
    #[serde(rename = "id")]
    short_id: ProfileId,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn create_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<CreateProfileResponse>)> {
    let id = generate_id(&state).await?;

    let profile = upload_profile(id, &user, body, true, &state).await?;

    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update_profile(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<ProfileId>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(&id, &user, &state).await?;

    let profile = upload_profile(id, &user, body, false, &state).await?;

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

    commit_s3_tx(tx, state.storage.delete(storage_key(&id))).await?;

    state.sockets.notify_profile_deleted(id);

    Ok(StatusCode::NO_CONTENT)
}

async fn upload_profile(
    id: ProfileId,
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

    let mods_json = serde_json::to_value(&manifest.mods)
        .map_err(|err| anyhow!("failed to serialize mods: {err}"))?;

    let mut tx = state.db.begin().await?;

    let profile = sqlx::query_as!(
        CreateProfileResponse,
        r#"INSERT INTO profiles (short_id, owner_id, name, community, mods)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT(short_id)
        DO UPDATE SET
            name = EXCLUDED.name,
            mods = EXCLUDED.mods,
            updated_at = NOW()
        RETURNING
            short_id AS "short_id: ProfileId", 
            created_at,
            updated_at"#,
        &*id.as_str(),
        user.id,
        manifest.profile_name,
        manifest.community,
        mods_json
    )
    .fetch_one(&mut *tx)
    .await?;

    commit_s3_tx(
        tx,
        state
            .storage
            .upload(storage_key(&profile.short_id), body, post),
    )
    .await?;

    state.sockets.notify_profile_updated(ProfileMetadata {
        short_id: id,
        created_at: profile.created_at,
        updated_at: profile.updated_at,
        owner: user.clone(),
        manifest,
    });

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

/// This makes sure an s3 operation and a database transaction
/// happens atomically (by rolling back the db if s3 failed).
///
/// It's not perfect, since if the final database commit fails
/// s3 will still go through.
async fn commit_s3_tx<T, Fut>(tx: sqlx::Transaction<'_, Postgres>, s3: Fut) -> AppResult<T>
where
    Fut: Future<Output = AppResult<T>>,
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
    Path(id): Path<ProfileId>,
    State(state): State<AppState>,
) -> AppResult<Redirect> {
    let profile = sqlx::query!(
        "UPDATE profiles
            SET downloads = downloads + 1
        WHERE short_id = $1
        RETURNING updated_at",
        &*id.as_str()
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Include a versioned query param to make sure we aren't getting
    // an already cached old version
    let url = state.storage.object_url(format!(
        "{}?v={}",
        storage_key(&id),
        profile.updated_at.timestamp()
    ));

    Ok(Redirect::to(&url))
}

async fn get_profile_metadata(
    State(state): State<AppState>,
    Path(id): Path<ProfileId>,
) -> AppResult<Json<ProfileMetadata>> {
    let profile = sqlx::query!(
        r#"SELECT
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
        WHERE p.short_id = $1"#,
        &id.to_string()
    )
    .map(|record| ProfileMetadata {
        short_id: id.clone(),
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

fn storage_key(id: &ProfileId) -> String {
    format!(
        "profile/{}.zip",
        match id {
            ProfileId::Legacy(short_uuid) => &short_uuid.0 as &dyn Display,
            ProfileId::Short(short) => short,
        }
    )
}
