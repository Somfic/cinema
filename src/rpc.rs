use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::app::Error;

pub struct RpcResponse<T>(pub Result<T, Error>);

pub fn ok<T>(r: Result<T, Error>) -> RpcResponse<T> {
    RpcResponse(r)
}

impl<T: Serialize> IntoResponse for RpcResponse<T> {
    fn into_response(self) -> Response {
        match self.0 {
            Ok(v) => Json(v).into_response(),
            Err(e) => {
                let (status, kind) = error_kind(&e);
                let body = serde_json::json!({
                    "kind": kind,
                    "message": e.to_string(),
                });
                (status, Json(body)).into_response()
            }
        }
    }
}

fn error_kind(e: &Error) -> (StatusCode, &'static str) {
    match e {
        Error::NotFound(_) => (StatusCode::NOT_FOUND, "NotFound"),
        Error::Generic(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal"),
        Error::TomlError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Config"),
        Error::ConfigReadError { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "Config"),
        Error::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database"),
        Error::HttpClientError(_) => (StatusCode::BAD_GATEWAY, "Upstream"),
        Error::IoError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Io"),
        Error::MigrationError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Migration"),
        Error::AddressParseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal"),
        Error::JsonError(_) => (StatusCode::BAD_REQUEST, "InvalidJson"),
    }
}
