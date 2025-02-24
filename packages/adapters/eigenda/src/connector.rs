use futures::{stream, StreamExt, TryFutureExt};
use services::{
    types::{DispersalStatus, EigenDASubmission, Fragment},
    Error as ServiceError, Result as ServiceResult,
};
use sha3::{Digest, Keccak256};
use signers::KeySource;
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
    error::{Error, Result},
    signer::EigenDASigner,
};

impl services::state_committer::port::eigen_da::Api for EigenDAClient {
    async fn submit_state_fragment(&self, fragment: Fragment) -> ServiceResult<EigenDASubmission> {
        let data: Vec<_> = fragment.data.into_iter().collect();
        let data = convert_by_padding_empty_byte(&data);

        let mut client = self.clone();
        let request_id = client
            .handle_authenticated_dispersal(data)
            .map_err(|e| ServiceError::Other(format!("Failed to disperse state fragment: {e}")))
            .await?;

        Ok(EigenDASubmission {
            request_id,
            created_at: None,
            status: DispersalStatus::Processing,
            ..Default::default()
        })
    }
}

impl services::state_listener::port::eigen_da::Api for EigenDAClient {
    fn get_blob_status(
        &self,
        id: Vec<u8>,
    ) -> impl ::core::future::Future<Output = ServiceResult<DispersalStatus>> + Send {
        async move {
            let status = self.check_status(id).await?;
            Ok(status)
        }
    }
}

#[derive(Debug, Clone)]
pub struct EigenDAClient {
    client: DisperserClient<Channel>,
    signer: EigenDASigner,
    account_id: String,
}

impl EigenDAClient {
    pub async fn new(key: KeySource, rpc: Url) -> Result<Self> {
        let tls_config = ClientTlsConfig::new().with_native_roots();

        let endpoint = Channel::from_shared(rpc.to_string())
            .map_err(|_| Error::Other("failed to parse url".to_string()))?
            .tls_config(tls_config)?
            .connect()
            .await?;

        let client = DisperserClient::new(endpoint);

        let signer = EigenDASigner::new(key).await?;
        let account_id = signer.account_id().await;

        Ok(Self {
            client,
            signer,
            account_id,
        })
    }

    async fn sign_challenge_u32(&self, nonce: u32) -> Result<Vec<u8>> {
        let nonce_bytes = nonce.to_be_bytes();
        let hash = Keccak256::digest(nonce_bytes);

        let signature = self.signer.sign_prehash(&hash).await?;

        Ok(signature)
    }

    async fn handle_authenticated_dispersal(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
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
            .map_err(|_| Error::AuthenticationFailed)?;

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
                        .map_err(|_| Error::AuthenticationFailed)?;
                }
                Some(authenticated_reply::Payload::DisperseReply(reply)) => {
                    return Ok(reply.request_id);
                }
                None => {
                    return Err(Error::Other("received unexpected response".to_string()));
                }
            }
        }

        Err(Error::AuthenticationFailed)
    }

    async fn handle_unauthenticated_dispersal(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
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

    async fn check_status(&self, request_id: Vec<u8>) -> Result<DispersalStatus> {
        let mut client = self.client.clone();
        let status = client
            .get_blob_status(Request::new(BlobStatusRequest { request_id }))
            .await?
            .into_inner()
            .status;

        let status = BlobStatus::try_from(status).unwrap_or(BlobStatus::Unknown);

        Ok(status.into())
    }
}
