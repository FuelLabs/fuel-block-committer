pub mod aws;

pub use aws::{AwsConfig, AwsKmsClient};

#[derive(Debug, Clone, PartialEq)]
pub enum KeySource {
    Kms(String),
    Private(String),
}

impl KeySource {
    pub fn raw(&self) -> &str {
        match self {
            KeySource::Kms(k) => k,
            KeySource::Private(k) => k,
        }
    }
}

impl<'a> serde::Deserialize<'a> for KeySource {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let value = String::deserialize(deserializer)?;
        if let Some(k) = value.strip_prefix("Kms(").and_then(|s| s.strip_suffix(')')) {
            Ok(KeySource::Kms(k.to_string()))
        } else if let Some(k) = value
            .strip_prefix("Private(")
            .and_then(|s| s.strip_suffix(')'))
        {
            Ok(KeySource::Private(k.to_string()))
        } else {
            Err(serde::de::Error::custom("invalid KeySource format"))
        }
    }
}
