use std::{
    collections::HashMap,
    ffi::OsStr,
    io::{Cursor, Read, Seek, Write},
};

use anyhow::{anyhow, Context};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    response::Response,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use http::{HeaderValue, StatusCode};
use rand::Rng;
use serde::Serialize;
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

use crate::{
    auth::{self, AuthUser},
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
    State(mut state): State<AppState>,
    body: Bytes,
) -> AppResult<(StatusCode, Json<CreateProfileResponse>)> {
    let id = generate_id(&state).await?;

    let profile = upload_profile(id, &user, body, &mut state).await?;

    Ok((StatusCode::CREATED, Json(profile)))
}

async fn update_profile(
    AuthUser(user): AuthUser,
    State(mut state): State<AppState>,
    Path(id): Path<ProfileId>,
    body: Bytes,
) -> AppResult<Json<CreateProfileResponse>> {
    check_permission(&id, &user, &state).await?;

    let profile = upload_profile(id, &user, body, &mut state).await?;

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

    tx.commit().await?;

    state
        .sockets
        .notify_profile_deleted(state.redis.clone(), &id);

    Ok(StatusCode::NO_CONTENT)
}

async fn upload_profile(
    id: ProfileId,
    user: &auth::User,
    body: Bytes,
    state: &mut AppState,
) -> Result<CreateProfileResponse, AppError> {
    let cursor = Cursor::new(body.clone());
    // reading the zip file could be intensive
    let (manifest, configs) = tokio::task::spawn_blocking(|| read_manifest(cursor))
        .await
        .map_err(|err| anyhow!(err))??;

    let mods_json = serde_json::to_value(&manifest.mods)
        .map_err(|err| anyhow!("failed to serialize mods: {err}"))?;

    let configs_json = serde_json::to_value(&configs)
        .map_err(|err| anyhow!("failed to serialize configs: {err}"))?;

    let mut tx = state.db.begin().await?;

    let profile = sqlx::query_as!(
        CreateProfileResponse,
        r#"INSERT INTO profiles (short_id, owner_id, name, community, mods, configs)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT(short_id)
        DO UPDATE SET
            name = EXCLUDED.name,
            mods = EXCLUDED.mods,
            configs = EXCLUDED.configs,
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
        configs_json
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

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

const ALLOWED_EXTENSIONS: &[&str] = &["cfg", "txt", "json", "yml", "yaml", "ini", "xml"];

fn read_manifest(input: impl Read + Seek) -> AppResult<(ProfileManifest, HashMap<String, String>)> {
    let mut input_zip = ZipArchive::new(input)
        .map_err(|err| AppError::bad_request(format!("Invalid ZIP archive: {err}")))?;

    let mut files = HashMap::new();
    let mut manifest = None;

    for i in 0..input_zip.len() {
        let mut file = input_zip
            .by_index(i)
            .context("failed to open file in zip")?;

        let Some(path) = file.enclosed_name() else {
            return Err(AppError::bad_request(format!(
                "File at {} escapes ZIP archive.",
                file.name()
            )));
        };

        if file.name() == "export.r2x" {
            manifest = Some(
                serde_yml::from_reader::<_, ProfileManifest>(file).map_err(|err| {
                    AppError::bad_request(format!("Error parsing export.r2x: {err}"))
                })?,
            );
        } else {
            if !path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ALLOWED_EXTENSIONS.contains(&ext))
            {
                return Err(AppError::bad_request(format!(
                    "File at {} has a forbidden extension.",
                    path.display()
                )));
            }

            let mut str = String::with_capacity(file.size() as usize);
            file.read_to_string(&mut str)
                .context("failed to read file in zip")?;

            files.insert(file.name().to_string(), str);
        }
    }

    let manifest = manifest.ok_or_else(|| AppError::bad_request("Profile manifest is missing."))?;

    Ok((manifest, files))
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
) -> AppResult<Response> {
    let profile = sqlx::query!(
        r#"UPDATE profiles
            SET downloads = downloads + 1
        WHERE short_id = $1
        RETURNING
            name,
            community,
            mods AS "mods: sqlx::types::Json<Vec<ProfileMod>>",
            configs AS "configs: sqlx::types::Json<HashMap<String, String>>",
            updated_at"#,
        &*id.as_str()
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let mut writer = Cursor::new(Vec::new());

    {
        let mut zip = ZipWriter::new(&mut writer);

        let manifest = ProfileManifest {
            profile_name: profile.name,
            community: profile.community,
            mods: profile.mods.0,
        };

        zip.start_file("export.r2x", SimpleFileOptions::default())
            .context("failed to start writing manifest")?;
        serde_yml::to_writer(&mut zip, &manifest).context("error writing manifest")?;

        if let Some(configs) = profile.configs {
            for (path, content) in configs.0 {
                zip.start_file(&path, SimpleFileOptions::default())
                    .with_context(|| format!("failed to start writing config file at {path}"))?;

                zip.write_all(content.as_bytes())
                    .with_context(|| format!("error writing config file at {path}"))?;
            }
        }
    }

    let mut response = Response::new(Body::from(writer.into_inner()));

    response
        .headers_mut()
        .append("Content-Type", HeaderValue::from_static("application/zip"));

    Ok(response)
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

        if !exists {
            return Ok(ProfileId::Short(id));
        }
    }
}
