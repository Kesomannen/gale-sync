use std::fmt::Display;

use base64::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A base64-encoded UUID to shorten profile ids
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
