use sqlx::types::chrono::{DateTime, Utc};

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn elapsed(&self, since: DateTime<Utc>) -> Result<std::time::Duration, String> {
        let elapsed = self.now().signed_duration_since(since);
        elapsed
            .to_std()
            .map_err(|e| format!("failed to convert time: {}", e))
    }
}
