use actix_web::ResponseError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Other(String),
    #[error("Network Error: {0}")]
    NetworkError(String),
}

impl ResponseError for Error {}

pub type Result<T> = std::result::Result<T, Error>;
