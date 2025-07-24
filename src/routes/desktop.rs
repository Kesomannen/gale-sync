use axum::{extract::Path, response::Redirect, routing::get, Router};

use crate::prelude::*;

pub fn router() -> Router<AppState> {
    Router::new().route("/{*path}", get(handler))
}

async fn handler(Path(path): Path<String>) -> Redirect {
    Redirect::to(&format!("gale://{path}"))
}
