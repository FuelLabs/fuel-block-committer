use crate::{Error, Result};

use sqlx::types::chrono::{DateTime, Utc};

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn elapsed(&self, since: DateTime<Utc>) -> Result<std::time::Duration> {
        self.now()
            .signed_duration_since(since)
            .to_std()
            .map_err(|e| Error::Other(format!("failed to convert time: {}", e)))
    }
}
