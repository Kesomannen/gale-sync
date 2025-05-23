use std::fmt::Display;

use base64::prelude::*;
use serde::{Deserialize, Serialize};
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef},
    Decode, Encode, Postgres, Type,
};
use uuid::Uuid;

/// A base64-encoded UUID
///
/// This is used to shorten profile ids and make them more distinct
/// from Thunderstore legacyprofile ids.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ShortUuid(pub Uuid);

impl From<Uuid> for ShortUuid {
    fn from(value: Uuid) -> Self {
        ShortUuid(value)
    }
}

impl Display for ShortUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&BASE64_URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl From<ShortUuid> for String {
    fn from(value: ShortUuid) -> Self {
        format!("{value}")
    }
}

impl TryFrom<String> for ShortUuid {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bytes = BASE64_URL_SAFE_NO_PAD.decode(&value)?;
        let uuid = Uuid::from_slice(&bytes)?;
        Ok(ShortUuid(uuid))
    }
}

// Using derive(sqlx::Type) doesn't correctly recognize
// the Postgres type as an UUID, so we have to do the impls ourselves

impl Type<Postgres> for ShortUuid {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        Uuid::type_info()
    }
}

impl PgHasArrayType for ShortUuid {
    fn array_type_info() -> PgTypeInfo {
        Uuid::array_type_info()
    }
}

impl Encode<'_, Postgres> for ShortUuid {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        self.0.encode_by_ref(buf)
    }
}

impl Decode<'_, Postgres> for ShortUuid {
    fn decode(value: PgValueRef<'_>) -> Result<Self, BoxDynError> {
        Uuid::decode(value).map(Self)
    }
}
