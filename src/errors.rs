use std::fmt::Display;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Network Error: {0}")]
    NetworkError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
