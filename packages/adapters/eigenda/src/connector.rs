use futures::{stream, StreamExt, TryFutureExt};
use services::{
    types::{Fragment, NonEmpty},
    Error as ServiceError, Result as ServiceResult,
};
use sha3::{Digest, Keccak256};
use signers::KeySource;
use std::{cmp::min, time::Duration};
use tokio::time::sleep;
use tonic::{
    transport::{Channel, ClientTlsConfig},
    Request,
};
use url::Url;

use crate::{
    bindings::{
        authenticated_reply,
        disperser::{
            authenticated_request, disperser_client::DisperserClient, AuthenticatedRequest,
            BlobStatus, BlobStatusRequest, DisperseBlobRequest,
        },
        AuthenticationData,
    },
    codec::convert_by_padding_empty_byte,
    error::EigenDAError,
    signer::EigenDASigner,
};

pub trait DataAvailabilityConnector: Send + Sync {
    fn post_data(
        &self,
        data: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, EigenDAError>> + Send;
    fn check_status(
        &self,
        request_id: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<BlobStatus, EigenDAError>> + Send;
    fn wait_for_finalization(
        &self,
        request_id: Vec<u8>,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<BlobStatus, EigenDAError>> + Send;
}

pub(crate) fn serialize_fragments(fragments: impl IntoIterator<Item = Fragment>) -> Vec<u8> {
    fragments.into_iter().fold(Vec::new(), |mut acc, fragment| {
        acc.extend_from_slice(&fragment.data.into_iter().collect::<Vec<_>>());
        acc
    })
}

// impl services::state_committer::port::l1::EigenDAApi for EigenDAClient {
//     async fn submit_state_fragments(
//         &self,
//         fragments: NonEmpty<Fragment>,
//     ) -> ServiceResult<Vec<u8>> {
//         let num_fragments = min(fragments.len(), 6);
//         let fragments = fragments.into_iter().take(num_fragments);
//         let data = serialize_fragments(fragments);

//         let data = convert_by_padding_empty_byte(&data);

//         let mut client = self.clone();
//         client
//             .handle_authenticated_dispersal(data)
//             .map_err(|e| ServiceError::Other(format!("Failed to disperse state fragments: {e}")))
//             .await
//     }
// }

#[derive(Debug, Clone)]
pub struct EigenDAClient {
    client: DisperserClient<Channel>,
    signer: EigenDASigner,
    account_id: String,
}

impl EigenDAClient {
    pub async fn new(key: KeySource, rpc: Url) -> Result<Self, EigenDAError> {
        let tls_config = ClientTlsConfig::new().with_native_roots();

        let endpoint = Channel::from_shared(rpc.to_string())
            .map_err(|_| EigenDAError::Other("failed to parse url".to_string()))?
            .tls_config(tls_config)?
            .connect()
            .await?;

        let client = DisperserClient::new(endpoint);

        let signer = EigenDASigner::new(key).await;
        let account_id = signer.account_id().await;

        Ok(Self {
            client,
            signer,
            account_id,
        })
    }

    async fn sign_challenge_u32(&self, nonce: u32) -> Result<Vec<u8>, EigenDAError> {
        let nonce_bytes = nonce.to_be_bytes();
        let hash = Keccak256::digest(nonce_bytes);

        let signature = self.signer.sign_prehash(&hash).await?;

        Ok(signature)
    }

    async fn handle_authenticated_dispersal(
        &mut self,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, EigenDAError> {
        let disperse_request = AuthenticatedRequest {
            payload: Some(authenticated_request::Payload::DisperseRequest(
                DisperseBlobRequest {
                    data,
                    custom_quorum_numbers: vec![],
                    account_id: self.account_id.clone(),
                },
            )),
        };

        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let tx_clone = tx.clone();

        tx.send(disperse_request)
            .await
            .map_err(|_| EigenDAError::AuthenticationFailed)?;

        // Start bidirectional streaming with a stream that can be extended
        let mut stream = self
            .client
            .disperse_blob_authenticated(Request::new(stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|req| (req, rx))
            })))
            .await?
            .into_inner();

        // Process server responses
        while let Some(reply) = stream.next().await {
            let reply = reply?;

            match reply.payload {
                Some(authenticated_reply::Payload::BlobAuthHeader(header)) => {
                    let signature_bytes =
                        self.sign_challenge_u32(header.challenge_parameter).await?;

                    // Send back the authentication data
                    let auth_request = AuthenticatedRequest {
                        payload: Some(authenticated_request::Payload::AuthenticationData(
                            AuthenticationData {
                                authentication_data: signature_bytes,
                            },
                        )),
                    };

                    // Send the authentication response through the same stream
                    tx_clone
                        .send(auth_request)
                        .await
                        .map_err(|_| EigenDAError::AuthenticationFailed)?;
                }
                Some(authenticated_reply::Payload::DisperseReply(reply)) => {
                    return Ok(reply.request_id);
                }
                None => {
                    return Err(EigenDAError::Other(
                        "received unexpected response".to_string(),
                    ));
                }
            }
        }

        Err(EigenDAError::AuthenticationFailed)
    }

    async fn handle_unauthenticated_dispersal(
        &mut self,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, EigenDAError> {
        let reply = self
            .client
            .disperse_blob(Request::new(DisperseBlobRequest {
                data,
                custom_quorum_numbers: vec![],
                account_id: self.account_id.clone(),
            }))
            .await?
            .into_inner();

        Ok(reply.request_id)
    }
}

impl DataAvailabilityConnector for EigenDAClient {
    async fn post_data(&self, data: Vec<u8>) -> Result<Vec<u8>, EigenDAError> {
        let mut client = self.clone();
        client.handle_authenticated_dispersal(data).await
    }

    async fn check_status(&self, request_id: Vec<u8>) -> Result<BlobStatus, EigenDAError> {
        let mut client = self.client.clone();
        let status = client
            .get_blob_status(Request::new(BlobStatusRequest { request_id }))
            .await?
            .into_inner()
            .status;

        Ok(BlobStatus::try_from(status).unwrap_or(BlobStatus::Unknown))
    }

    async fn wait_for_finalization(
        &self,
        request_id: Vec<u8>,
        timeout: Duration,
    ) -> Result<BlobStatus, EigenDAError> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                return Err(EigenDAError::Other(
                    "timeout waiting for finalization".to_string(),
                ));
            }

            let status = self.check_status(request_id.clone()).await?;
            match status {
                BlobStatus::Failed | BlobStatus::Finalized | BlobStatus::InsufficientSignatures => {
                    return Ok(status);
                }
                _ => {
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
