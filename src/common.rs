#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EthTxStatus {
    Pending,
    Committed,
    Aborted,
}
