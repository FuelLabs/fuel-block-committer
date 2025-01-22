use futures::StreamExt;
use k256::ecdsa::{signature::Signer, Signature, SigningKey};
use std::time::Duration;
use tokio::time::sleep;
use tonic::{
    transport::{Channel, ClientTlsConfig},
    Request,
};

use crate::bindings::{
    authenticated_reply,
    disperser::{
        authenticated_request, disperser_client::DisperserClient, AuthenticatedRequest, BlobStatus,
        BlobStatusRequest, DisperseBlobRequest,
    },
    AuthenticationData,
};

// Main trait for data availability connectors
pub trait DataAvailabilityConnector: Send + Sync {
    fn post_data(
        &mut self,
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

pub struct EigenDAClient {
    client: DisperserClient<Channel>,
    signing_key: SigningKey,
    account_id: String,
}

impl EigenDAClient {
    pub async fn new(endpoint: String, signing_key: SigningKey) -> Result<Self, ConnectorError> {
        // TODO: revisit this once we start using tls
        let tls_config = ClientTlsConfig::new().with_native_roots();

        let endpoint = Channel::from_shared(endpoint)
            .unwrap()
            .tls_config(tls_config)
            .unwrap()
            .connect()
            .await?;

        let client = DisperserClient::new(endpoint);

        let public_key = signing_key
            .verifying_key()
            .to_encoded_point(false);
        let account_id = format!("0x{}", hex::encode(public_key.as_bytes()));

        Ok(Self {
            client,
            signing_key,
            account_id,
        })
    }

    async fn handle_authenticated_dispersal(
        &mut self,
        data: Vec<u8>
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
            .disperse_blob_authenticated(Request::new(futures::stream::iter(vec![initial_request.clone()])))
            .await?
            .into_inner();

        // process server response
        if let Some(reply) = stream.next().await {
            let reply = reply?;
            match reply.payload {
                Some(authenticated_reply::Payload::BlobAuthHeader(header)) => {
                    let challenge = header.challenge_parameter;
                    let message = challenge.to_be_bytes();

                    let signature: Signature = self.signing_key.sign(&message);
                    let signature_bytes = signature.to_vec();

                    let auth_request = AuthenticatedRequest {
                        payload: Some(authenticated_request::Payload::AuthenticationData(
                            AuthenticationData {
                                authentication_data: signature_bytes,
                            },
                        )),
                    };

                    // send the authentication response
                    self.client
                        .disperse_blob_authenticated(Request::new(futures::stream::iter(vec![
                            initial_request, auth_request
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
    async fn post_data(&mut self, data: Vec<u8>) -> Result<Vec<u8>, ConnectorError> {
        self.handle_unauthenticated_dispersal(data)
            .await
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
                BlobStatus::Failed
                | BlobStatus::Finalized
                | BlobStatus::InsufficientSignatures => {
                    return Ok(status);
                }
                _ => {
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
