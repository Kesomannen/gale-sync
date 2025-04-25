use std::borrow::Cow;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

pub type AppResult<T> = Result<T, AppError>;

pub type CowStr = Cow<'static, str>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Not found.")]
    NotFound,

    #[error("{}", match reason {
        Some(reason) => reason,
        None => "Bad request."
    })]
    BadRequest { reason: Option<CowStr> },

    #[error("{}", match reason {
        Some(reason) => reason,
        None => "Unathorized."
    })]
    Unauthorized { reason: Option<CowStr> },

    #[error("Forbidden.")]
    Forbidden,

    #[error("Something went wrong.")]
    Sqlx(#[from] sqlx::Error),

    #[error("Something went wrong.")]
    Reqwest(#[from] reqwest::Error),

    #[error("Something went wrong.")]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn bad_request(reason: impl Into<CowStr>) -> Self {
        AppError::BadRequest {
            reason: Some(reason.into()),
        }
    }

    pub fn unauthorized(reason: impl Into<CowStr>) -> Self {
        AppError::Unauthorized {
            reason: Some(reason.into()),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest { .. } => StatusCode::BAD_REQUEST,
            AppError::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::Sqlx(_) | AppError::Reqwest(_) | AppError::Other(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match &self {
            AppError::Sqlx(err) => tracing::error!("database error: {err:#}"),
            AppError::Reqwest(err) => tracing::error!("http error: {err:#}"),
            AppError::Other(err) => tracing::error!("unexpected server error: {err:#}"),
            _ => (),
        }

        (
            self.status(),
            Json(ErrorResponse {
                message: self.to_string(),
            }),
        )
            .into_response()
    }
}

/*
pub trait ResultExt<T>
where
    Self: Sized,
{
    fn on_constraints<F>(self, map: F) -> AppResult<T>
    where
        F: FnOnce(&str) -> Option<AppError>;

    fn on_constraint<F>(self, constraint: &str, map: F) -> AppResult<T>
    where
        F: FnOnce() -> AppError,
    {
        self.on_constraints(|name| {
            if name == constraint {
                Some(map())
            } else {
                None
            }
        })
    }
}

impl<T> ResultExt<T> for Result<T, sqlx::Error> {
    fn on_constraints<F>(self, map: F) -> AppResult<T>
    where
        F: FnOnce(&str) -> Option<AppError>,
    {
        match self {
            Ok(res) => Ok(res),
            Err(err) => {
                if let sqlx::Error::Database(err) = &err {
                    if let Some(constraint) = err.constraint() {
                        if let Some(err) = map(constraint) {
                            return Err(err);
                        }
                    }
                }

                Err(AppError::Sqlx(err))
            }
        }
    }
}
    */
