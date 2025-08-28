use std::{borrow::Cow, fmt::Display};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef},
    prelude::*,
    Encode, Postgres, Type,
};

use crate::{auth::User, prelude::*, short_uuid::ShortUuid, AppState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(try_from = "String", into = "String")]
pub enum ProfileId {
    /// A 6-character alphanumeric string.
    Short(String),
    /// A base64-encoded UUID.
    ///
    /// Only here for compatibility with older profiles. New profiles should use the `Short` variant.
    Legacy(ShortUuid),
}

impl ProfileId {
    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            ProfileId::Short(short) => Cow::Borrowed(short),
            ProfileId::Legacy(uuid) => Cow::Owned(uuid.to_string()),
        }
    }
}

impl TryFrom<String> for ProfileId {
    type Error = AppError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if Regex::new(r"^[a-zA-Z0-9]{6}$").unwrap().is_match(&value) {
            Ok(ProfileId::Short(value))
        } else if let Ok(short_uuid) = ShortUuid::try_from(value.clone()) {
            Ok(ProfileId::Legacy(short_uuid))
        } else {
            Err(AppError::bad_request(format!(
                "Invalid profile id: {value}. Must be a base-64 encoded UUID or a 6-character alphanumeric string."
            )))
        }
    }
}

impl From<ProfileId> for String {
    fn from(value: ProfileId) -> Self {
        match value {
            ProfileId::Legacy(uuid) => uuid.to_string(),
            ProfileId::Short(short) => short,
        }
    }
}

impl Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileId::Legacy(uuid) => write!(f, "{uuid}",),
            ProfileId::Short(short) => write!(f, "{short}",),
        }
    }
}

impl Encode<'_, Postgres> for ProfileId {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<'_, Postgres>::encode_by_ref(&self.to_string(), buf)
    }

    fn encode(
        self,
        buf: &mut <Postgres as sqlx::Database>::ArgumentBuffer<'_>,
    ) -> Result<IsNull, BoxDynError>
    where
        Self: Sized,
    {
        Encode::<'_, Postgres>::encode(String::from(self), buf)
    }
}

impl Type<Postgres> for ProfileId {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl PgHasArrayType for ProfileId {
    fn array_type_info() -> PgTypeInfo {
        String::array_type_info()
    }
}

impl Decode<'_, Postgres> for ProfileId {
    fn decode(value: PgValueRef<'_>) -> Result<Self, BoxDynError> {
        <String as Decode<'_, Postgres>>::decode(value)
            .and_then(|str| Self::try_from(str).map_err(|err| err.into()))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMod {
    pub name: String,
    pub enabled: bool,
    pub version: ModVersion,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileManifest {
    pub profile_name: String,
    #[serde(default)]
    pub community: Option<String>,
    pub mods: Vec<ProfileMod>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMetadata {
    #[serde(rename = "id")]
    pub short_id: ProfileId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub owner: User,
    pub manifest: ProfileManifest,
}

pub async fn get(state: &AppState, id: &ProfileId) -> AppResult<Option<ProfileMetadata>> {
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
    .await?;

    Ok(profile)
}
