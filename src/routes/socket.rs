use axum::{
    extract::{State, WebSocketUpgrade},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use http::StatusCode;

use crate::prelude::*;

pub fn routes() -> Router<AppState> {
    Router::new().route("/connect", any(connect))
}

async fn connect(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    StatusCode::NOT_FOUND.into_response()
    //ws.on_upgrade(move |socket| crate::socket::handle(socket, state.sockets))
}
