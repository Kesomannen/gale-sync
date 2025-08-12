use std::borrow::Cow;

use axum::response::Html;

pub struct RedirectBuilder<'a> {
    title: Option<Cow<'a, str>>,
    description: Option<Cow<'a, str>>,
    image: Option<Cow<'a, str>>,
    url: Cow<'a, str>,
}

impl<'a> RedirectBuilder<'a> {
    pub fn new(url: impl Into<Cow<'a, str>>) -> Self {
        Self {
            title: None,
            description: None,
            image: None,
            url: url.into(),
        }
    }

    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<Cow<'a, str>>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn image(mut self, image: impl Into<Cow<'a, str>>) -> Self {
        self.image = Some(image.into());
        self
    }

    pub fn build(self) -> Html<String> {
        let mut html = include_str!("../assets/redirect.html").replace("%REDIRECT_URL%", &self.url);

        if let Some(title) = self.title {
            html = html.replace("%TITLE%", &title);
        }
        if let Some(description) = self.description {
            html = html.replace("%DESCRIPTION%", &description);
        }
        if let Some(image) = self.image {
            html = html.replace("%IMAGE%", &image);
        }

        Html(html)
    }
}
