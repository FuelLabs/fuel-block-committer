use std::{
    cmp::min, num::NonZeroU32, ops::RangeInclusive, str::FromStr, sync::Arc, time::Duration,
};

use crate::estimation::{MaxTxFeesPerGas, TransactionRequestExt};
use ::metrics::RegistersMetrics;
use alloy::{
    consensus::{SignableTransaction, Transaction},
    eips::{
        BlockNumberOrTag,
        eip4844::{BYTES_PER_BLOB, DATA_GAS_PER_BLOB},
    },
    network::{Ethereum, EthereumWallet, TransactionBuilder, TransactionBuilder4844, TxSigner},
    primitives::{Address, B256, ChainId},
    providers::{
        Provider, ProviderBuilder, SendableTx, WsConnect,
        utils::{EIP1559_FEE_ESTIMATION_PAST_BLOCKS, Eip1559Estimation},
    },
    pubsub::PubSubFrontend,
    rpc::types::{FeeHistory, TransactionReceipt, TransactionRequest},
    signers::{Signature, local::PrivateKeySigner},
    sol,
};
use itertools::Itertools;
use metrics::prometheus::{self, histogram_opts};
use serde::Deserialize;
use services::{
    state_committer::port::l1::Priority,
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
    },
};
use tracing::info;
use url::Url;

use crate::{
    AwsClient, AwsConfig, Error, Result, blob_encoder,
    failover_client::{ProviderConfig, ProviderInit},
    provider::L1Provider,
};
#[derive(Clone)]
pub struct Metrics {
    pub(crate) blobs_per_tx: prometheus::Histogram,
    pub(crate) blob_unused_bytes: prometheus::Histogram,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            blobs_per_tx: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_per_tx",
                "Number of blobs per blob transaction",
                vec![1.0f64, 2., 3., 4., 5., 6.]
            ))
            .expect("to be correctly configured"),

            blob_unused_bytes: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_unused_bytes",
                "unused bytes per blob",
                metrics::custom_exponential_buckets(1000f64, BYTES_PER_BLOB as f64, 20)
            ))
            .expect("to be correctly configured"),
        }
    }
}
