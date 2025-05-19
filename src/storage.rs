use std::{fmt::Display, sync::Arc};

use http::Method;

use crate::prelude::*;

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

    pub(crate) async fn upload(
        &self,
        key: impl Display,
        body: impl Into<reqwest::Body>,
        post: bool,
    ) -> AppResult<()> {
        let method = if post { Method::POST } else { Method::PUT };

        self.request(self.object_path(key), method)
            .body(body)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub(crate) async fn delete(&self, key: impl Display) -> AppResult<()> {
        self.request(self.object_path(key), Method::DELETE)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub(crate) fn url(&self, key: impl Display) -> String {
        format!("{}{}", self.base_url, self.object_path(key))
    }
}
