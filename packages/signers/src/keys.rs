#[derive(Debug, Clone, PartialEq)]
pub enum L1Key {
    Kms(String),
    Private(String),
}

impl<'a> serde::Deserialize<'a> for L1Key {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let value = String::deserialize(deserializer)?;
        if let Some(k) = value.strip_prefix("Kms(").and_then(|s| s.strip_suffix(')')) {
            Ok(L1Key::Kms(k.to_string()))
        } else if let Some(k) = value
            .strip_prefix("Private(")
            .and_then(|s| s.strip_suffix(')'))
        {
            Ok(L1Key::Private(k.to_string()))
        } else {
            Err(serde::de::Error::custom("invalid L1Key format"))
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct L1Keys {
    /// The key authorized by the L1 contracts to post commitments.
    pub main: L1Key,
    /// The key for posting data to L1.
    pub blob: Option<L1Key>,
}

impl L1Keys {
    pub fn uses_aws(&self) -> bool {
        matches!(self.main, L1Key::Kms(_)) || matches!(self.blob, Some(L1Key::Kms(_)))
    }
}