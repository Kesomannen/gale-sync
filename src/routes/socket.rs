use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
    routing::any,
    Router,
};

use crate::prelude::*;

pub fn routes() -> Router<AppState> {
    Router::new().route("/connect", any(connect))
}

async fn connect(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| crate::socket::handle(socket, state))
}
