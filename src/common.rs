use strum::{Display, EnumString};

#[derive(Debug, Clone,  PartialEq, Eq, EnumString, Display)]
pub enum EthTxStatus {
    Pending,
    Committed,
    Aborted,
}
