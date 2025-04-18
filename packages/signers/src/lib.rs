pub mod aws;
pub mod eigen_aws_kms;

pub use aws::{AwsConfig, AwsKmsClient};

#[derive(Debug, Clone, PartialEq)]
pub enum KeySource {
    Kms(String),
    Private(String),
}

impl ToString for KeySource {
    fn to_string(&self) -> String {
        let k = match self {
            KeySource::Kms(k) => k,
            KeySource::Private(k) => k,
        };
        k.to_owned()
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

#[cfg(test)]
mod tests {
    use super::KeySource;

    #[test]
    fn can_deserialize_private_key() {
        // given
        let val = r#""Private(0x1234)""#;

        // when
        let key: KeySource = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, KeySource::Private("0x1234".to_owned()));
    }

    #[test]
    fn can_deserialize_kms_key() {
        // given
        let val = r#""Kms(0x1234)""#;

        // when
        let key: KeySource = serde_json::from_str(val).unwrap();

        // then
        assert_eq!(key, KeySource::Kms("0x1234".to_owned()));
    }
}
