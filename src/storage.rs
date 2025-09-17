use std::{fmt::Display, sync::Arc};

use axum::body::Bytes;
use http::Method;

/// A client to interact with the Supabase storage API.
///
/// I couldn't find any good crates for this so I made my own :)
#[derive(Debug, Clone)]
pub struct Client {
    bucket_name: Arc<str>,
    api_key: Arc<str>,
    base_url: Arc<str>,
    http: reqwest::Client,
}

impl Client {
    pub fn new(
        bucket_name: Arc<str>,
        api_key: Arc<str>,
        base_url: Arc<str>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            bucket_name,
            api_key,
            base_url,
            http,
        }
    }

    fn object_path(&self, key: impl Display) -> String {
        format!("/object/{}/{}", self.bucket_name, key)
    }

    fn request(&self, path: impl Display, method: http::Method) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.http.request(method, url).bearer_auth(&*self.api_key)
    }

    pub(crate) async fn download(&self, key: impl Display) -> anyhow::Result<Bytes> {
        let bytes = self
            .request(self.object_path(key), Method::GET)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        Ok(bytes)
    }

    pub(crate) fn object_url(&self, key: impl Display) -> String {
        format!("{}{}", self.base_url, self.object_path(key))
    }
}
