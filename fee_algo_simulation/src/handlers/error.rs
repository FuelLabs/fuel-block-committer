use actix_web::HttpResponse;
use actix_web::ResponseError;
use thiserror::Error;
use tracing::error;

#[derive(Error, Debug)]
pub enum FeeError {
    #[error("Internal Server Error: {0}")]
    InternalError(String),
    #[error("Bad Request: {0}")]
    BadRequest(String),
}

impl ResponseError for FeeError {
    fn error_response(&self) -> HttpResponse {
        match self {
            FeeError::InternalError(msg) => HttpResponse::InternalServerError().body(msg.clone()),
            FeeError::BadRequest(msg) => HttpResponse::BadRequest().body(msg.clone()),
        }
    }
}
