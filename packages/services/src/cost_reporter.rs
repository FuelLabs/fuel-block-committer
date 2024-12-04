pub mod service {

    use crate::{types::BundleCost, Error, Result};

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
        Db: crate::cost_reporter::port::Storage,
    {
        pub async fn get_costs(
            &self,
            from_block_height: u32,
            limit: usize,
        ) -> Result<Vec<BundleCost>> {
            if limit > self.request_limit {
                return Err(Error::Other(format!(
                    "requested: {} items, but limit is: {}",
                    limit, self.request_limit
                )));
            }

            self.storage
                .get_finalized_costs(from_block_height, limit)
                .await
        }
    }
}

pub mod port {
    use crate::{types::BundleCost, Result};

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn get_finalized_costs(
            &self,
            from_block_height: u32,
            limit: usize,
        ) -> Result<Vec<BundleCost>>;
    }
}
