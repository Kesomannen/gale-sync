use axum::{extract::Path, response::Html, routing::get, Router};

use crate::{prelude::*, redirect::RedirectBuilder};

pub fn routes() -> Router<AppState> {
    Router::new().route("/profile/sync/clone/{id}", get(clone_profile))
}

async fn clone_profile(Path(id): Path<String>) -> Html<String> {
    RedirectBuilder::new(format!("gale://profile/sync/clone/{id}"))
        .title("Import sync profile")
        .description(id)
        .image("https://github.com/Kesomannen/gale/blob/master/images/icons/app-icon@0,25x.png?raw=true")
        .build()
}
