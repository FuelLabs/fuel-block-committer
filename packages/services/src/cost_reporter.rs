use ports::{storage::Storage, types::BundleCost};

use crate::{Error, Result};

pub struct CostReporter<Db> {
    storage: Db,
    request_limit: usize,
}

impl<Db> CostReporter<Db> {
    pub fn new(storage: Db, request_limit: usize) -> Self {
        Self {
            storage,
            request_limit,
        }
    }
}

impl<Db> CostReporter<Db>
where
    Db: Storage,
{
    pub async fn get_costs(&self, from_block_height: u32, limit: usize) -> Result<Vec<BundleCost>> {
        if limit > self.request_limit {
            return Err(Error::Other(format!(
                "requested: {} items, but limit is: {}",
                limit, self.request_limit
            )));
        }

        Ok(self
            .storage
            .get_finalized_costs(from_block_height, limit)
            .await?)
    }
}
