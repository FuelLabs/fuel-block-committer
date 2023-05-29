use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum EthTxStatus {
    Pending,
    Commited,
    Aborted,
}
