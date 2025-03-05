use prost::Message as _;
use services::{
    types::{DispersalStatus, EigenDASubmission, Fragment, Utc},
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
        common::{BlobCommitment, BlobHeader, PaymentHeader},
        disperser::{
            disperser_client::DisperserClient, BlobStatus, BlobStatusRequest, DisperseBlobRequest,
        },
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
            .post_data(data)
            .await
            .map_err(|e| ServiceError::Other(format!("Failed to disperse state fragment: {e}")))?;

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

    async fn post_data(&mut self, data: Vec<u8>) -> Result<Vec<u8>> {
        let blob_header = self.blob_header(&data);
        let signature = self.sign_blob_header(&blob_header).await?;

        let request = DisperseBlobRequest {
            blob: data,
            blob_header: Some(blob_header),
            signature,
        };

        let response = self.client.disperse_blob(Request::new(request)).await?;
        let reply = response.into_inner();

        if reply.result != BlobStatus::Queued as i32 {
            return Err(Error::Other(format!(
                "Unexpected result during dispersal: {}",
                reply.result
            )));
        }

        Ok(reply.blob_key)
    }

    // TODO
    fn compute_kzg_commitment_proof(data: &[u8]) -> BlobCommitment {
        // For demonstration, use Keccak256 hashes as dummy values.
        let commitment = Keccak256::digest(data).to_vec();
        let length_commitment = Keccak256::digest(&[data.len() as u8]).to_vec();
        let length_proof = Keccak256::digest(&[data.len() as u8, 0x01]).to_vec();
        BlobCommitment {
            commitment,
            length_commitment,
            length_proof,
            length: data.len() as u32,
        }
    }

    fn blob_header(&self, data: &[u8]) -> BlobHeader {
        let blob_commitment = Self::compute_kzg_commitment_proof(data);

        BlobHeader {
            version: 2, // TODO: verify this is correct
            quorum_numbers: vec![],
            commitment: Some(blob_commitment),
            payment_header: Some(PaymentHeader {
                account_id: self.account_id.clone(),
                timestamp: Utc::now().timestamp_nanos_opt().expect("timestampt failed"),
                cumulative_payment: vec![], // TODO: cumulative payment as required.
            }),
        }
    }

    async fn sign_blob_header(&self, header: &BlobHeader) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(header.encoded_len());
        header
            .encode(&mut buf)
            .expect("Failed to encode blob header");

        let hash = Keccak256::digest(&buf);

        let signature = self.signer.sign_prehash(&hash).await?;

        Ok(signature)
    }

    async fn check_status(&self, blob_key: Vec<u8>) -> Result<DispersalStatus> {
        let request = BlobStatusRequest { blob_key };
        let mut client = self.client.clone();

        let response = client.get_blob_status(Request::new(request)).await?;
        let reply = response.into_inner();

        let status = BlobStatus::try_from(reply.status).unwrap_or(BlobStatus::Unknown);
        Ok(status.into())
    }
}
