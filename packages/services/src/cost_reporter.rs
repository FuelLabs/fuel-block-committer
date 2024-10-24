use ports::{storage::Storage, types::BundleCost};

use crate::Result;

pub struct CostReporter<Db> {
    storage: Db,
}

impl<Db> CostReporter<Db> {
    pub fn new(storage: Db) -> Self {
        Self { storage }
    }
}

impl<Db> CostReporter<Db>
where
    Db: Storage,
{
    pub async fn get_costs(&self, from_block_height: u32, limit: usize) -> Result<Vec<BundleCost>> {
        Ok(self
            .storage
            .get_finalized_costs(from_block_height, limit)
            .await?)
    }
}
