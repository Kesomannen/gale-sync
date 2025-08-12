use axum::{extract::Path, response::Html, routing::get, Router};

use crate::prelude::*;

pub fn routes() -> Router<AppState> {
    Router::new().route("/{*path}", get(handler))
}

async fn handler(Path(path): Path<String>) -> Html<String> {
    crate::redirect::to(&format!("gale://{path}"))
}
