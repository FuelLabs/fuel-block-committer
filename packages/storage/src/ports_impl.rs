use std::time::Duration;

use crate::Postgres;
use services::{state_pruner, Result};

impl state_pruner::port::Storage for Postgres {
    async fn prune_entries_older_than(&self, duration: Duration) -> Result<()> {
        Ok(())
    }
}
