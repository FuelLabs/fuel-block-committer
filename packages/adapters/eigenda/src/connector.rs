use futures::{StreamExt, TryFutureExt};
use k256::ecdsa::{signature::Signer, Signature, SigningKey};
use services::{
    types::{Fragment, NonEmpty},
    Error as ServiceError, Result as ServiceResult,
};
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
    error::ConnectorError,
    signer::EigenDASigner,
};

pub trait DataAvailabilityConnector: Send + Sync {
    fn post_data(
        &self,
        data: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, ConnectorError>> + Send;
    fn check_status(
        &self,
        request_id: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<BlobStatus, ConnectorError>> + Send;
    fn wait_for_finalization(
        &self,
        request_id: Vec<u8>,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<BlobStatus, ConnectorError>> + Send;
}

pub(crate) fn serialize_fragments(fragments: impl IntoIterator<Item = Fragment>) -> Vec<u8> {
    fragments.into_iter().fold(Vec::new(), |mut acc, fragment| {
        acc.extend_from_slice(&fragment.data.into_iter().collect::<Vec<_>>());
        acc
    })
}

impl services::state_committer::port::l1::DALayerApi for EigenDAClient {
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
    ) -> ServiceResult<Vec<u8>> {
        let num_fragments = min(fragments.len(), 6);
        let fragments = fragments.into_iter().take(num_fragments);
        let data = serialize_fragments(fragments);

        let data = convert_by_padding_empty_byte(&data);

        let mut client = self.clone();
        client
            .handle_authenticated_dispersal(data)
            .map_err(|e| ServiceError::Other(format!("Failed to disperse state fragments: {e}")))
            .await
    }
}

#[derive(Debug, Clone)]
pub struct EigenDAClient {
    client: DisperserClient<Channel>,
    signer: EigenDASigner,
    account_id: String,
}

impl EigenDAClient {
    pub async fn new(key: KeySource, rpc: Url) -> Result<Self, ConnectorError> {
        // TODO: revisit this once we start using tls
        let tls_config = ClientTlsConfig::new().with_native_roots();

        let endpoint = Channel::from_shared(rpc.to_string())
            .unwrap()
            .tls_config(tls_config)
            .unwrap()
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

    async fn handle_authenticated_dispersal(
        &mut self,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, ConnectorError> {
        // create initial request with blob data
        let initial_request = AuthenticatedRequest {
            payload: Some(authenticated_request::Payload::DisperseRequest(
                DisperseBlobRequest {
                    data,
                    custom_quorum_numbers: vec![], // ussing default quorums
                    account_id: self.account_id.clone(),
                },
            )),
        };

        let mut stream = self
            .client
            .disperse_blob_authenticated(Request::new(futures::stream::iter(vec![
                initial_request.clone()
            ])))
            .await?
            .into_inner();

        // process server response
        if let Some(reply) = stream.next().await {
            let reply = reply?;
            match reply.payload {
                Some(authenticated_reply::Payload::BlobAuthHeader(header)) => {
                    // send the authentication response
                    let challenge = header.challenge_parameter;
                    let message = challenge.to_be_bytes();

                    let signature = self.signer.sign(&message).await?;

                    let auth_request = AuthenticatedRequest {
                        payload: Some(authenticated_request::Payload::AuthenticationData(
                            AuthenticationData {
                                authentication_data: signature.data,
                            },
                        )),
                    };

                    self.client
                        .disperse_blob_authenticated(Request::new(futures::stream::iter(vec![
                            initial_request,
                            auth_request,
                        ])))
                        .await?;
                }
                Some(authenticated_reply::Payload::DisperseReply(reply)) => {
                    return Ok(reply.request_id);
                }
                None => {
                    return Err(ConnectorError::AuthenticationFailed);
                }
            }
        }

        Err(ConnectorError::AuthenticationFailed)
    }

    async fn handle_unauthenticated_dispersal(
        &mut self,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, ConnectorError> {
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
    async fn post_data(&self, data: Vec<u8>) -> Result<Vec<u8>, ConnectorError> {
        let mut client = self.clone();
        client.handle_authenticated_dispersal(data).await
    }

    async fn check_status(&self, request_id: Vec<u8>) -> Result<BlobStatus, ConnectorError> {
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
    ) -> Result<BlobStatus, ConnectorError> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                return Err(ConnectorError::Timeout);
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
