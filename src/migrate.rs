use axum::body::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

use crate::{prelude::*, profile::ProfileId};

async fn download_task(tx: mpsc::Sender<(Uuid, Bytes)>, state: AppState) -> anyhow::Result<()> {
    let mut profiles = sqlx::query!(
        r#"SELECT id, short_id AS "short_id: ProfileId" FROM profiles WHERE code IS NULL"#
    )
    .fetch(&state.db);

    while let Some(profile) = profiles.next().await.transpose()? {
        let path = crate::profile::storage_key(&profile.short_id);

        let archive = state.storage.download(path).await?;
        tx.send((profile.id, archive)).await?;
    }

    Ok(())
}

pub async fn migrate(state: &AppState) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel(32);

    let state2 = state.to_owned();
    tokio::spawn(async move {
        if let Err(err) = download_task(tx, state2).await {
            error!("download error: {err}")
        }
    });

    let mut count = 0;

    while let Some((profile_id, bytes)) = rx.recv().await {
        let key = crate::profile::upload(state, bytes).await?;

        sqlx::query!(
            "UPDATE profiles SET code = $1 WHERE id = $2",
            key,
            profile_id
        )
        .execute(&state.db)
        .await?;

        info!("migrated profile #{count}",);

        count += 1;
    }

    Ok(())
}
