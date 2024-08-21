use std::{ffi::CString, ops::Deref, sync::OnceLock};

use ethers::{
    core::k256::sha2::{Digest, Sha256},
    signers::{AwsSigner, Signer},
    types::{Address, Signature, H256, U256},
    utils::keccak256,
};
use rlp::RlpStream;

use crate::error::Error;

fn kzg_settings() -> &'static c_kzg::KzgSettings {
    static KZG_SETTINGS: OnceLock<c_kzg::KzgSettings> = OnceLock::new();
    KZG_SETTINGS.get_or_init(|| {
        // TODO: Load the trusted setup from static bytes.
        let temp_file = tempfile::NamedTempFile::new().expect("can create a temporary file");
        let trusted_setup = include_str!("trusted_setup.txt");
        std::fs::write(temp_file.path(), trusted_setup)
            .expect("can write trusted setup to temporary file");

        let stringified_path = temp_file
            .path()
            .as_os_str()
            .to_str()
            .expect("path is valid utf8");

        c_kzg::KzgSettings::load_trusted_setup_file(
            &CString::new(stringified_path).expect("C string"),
        )
        .unwrap()
    })
}

const BLOB_TX_TYPE: u8 = 0x03;
const MAX_BLOBS_PER_BLOCK: usize = 6;
pub const MAX_BYTES_PER_BLOB: usize = c_kzg::BYTES_PER_BLOB;

#[async_trait::async_trait]
pub trait BlobSigner {
    async fn sign_hash(&self, hash: H256) -> crate::error::Result<Signature>;
}

#[async_trait::async_trait]
impl BlobSigner for AwsSigner {
    async fn sign_hash(&self, hash: H256) -> crate::error::Result<Signature> {
        let sig = self
            .sign_digest(hash.into())
            .await
            .map_err(|err| crate::error::Error::Other(format!("Error signing digest: {err}")))?;

        let pub_key = &self.get_pubkey().await.map_err(|err| {
            crate::error::Error::Other(format!("Error getting pubkey to sign digest: {err}"))
        })?;

        let mut sig =
            copied_from_ethers::sig_from_digest_bytes_trial_recovery(&sig, hash.into(), pub_key);

        copied_from_ethers::apply_eip155(&mut sig, self.chain_id());
        Ok(sig)
    }
}

mod copied_from_ethers {
    use ethers::{
        core::k256::{
            ecdsa::{RecoveryId, VerifyingKey},
            FieldBytes,
        },
        types::Signature,
    };
    use ports::types::U256;

    /// Recover an rsig from a signature under a known key by trial/error
    pub(super) fn sig_from_digest_bytes_trial_recovery(
        sig: &ethers::core::k256::ecdsa::Signature,
        digest: [u8; 32],
        vk: &VerifyingKey,
    ) -> ethers::types::Signature {
        let r_bytes: FieldBytes = sig.r().into();
        let s_bytes: FieldBytes = sig.s().into();
        let r = U256::from_big_endian(r_bytes.as_slice());
        let s = U256::from_big_endian(s_bytes.as_slice());

        if check_candidate(sig, RecoveryId::from_byte(0).unwrap(), digest, vk) {
            Signature { r, s, v: 0 }
        } else if check_candidate(sig, RecoveryId::from_byte(1).unwrap(), digest, vk) {
            Signature { r, s, v: 1 }
        } else {
            panic!("bad sig");
        }
    }

    /// Makes a trial recovery to check whether an RSig corresponds to a known
    /// `VerifyingKey`
    fn check_candidate(
        sig: &ethers::core::k256::ecdsa::Signature,
        recovery_id: RecoveryId,
        digest: [u8; 32],
        vk: &VerifyingKey,
    ) -> bool {
        VerifyingKey::recover_from_prehash(digest.as_slice(), sig, recovery_id)
            .map(|key| key == *vk)
            .unwrap_or(false)
    }

    /// Modify the v value of a signature to conform to eip155
    pub(super) fn apply_eip155(sig: &mut Signature, chain_id: u64) {
        let v = (chain_id * 2 + 35) + sig.v;
        sig.v = v;
    }
}

pub struct PreparedBlob {
    pub commitment: Vec<u8>,
    pub proof: Vec<u8>,
    pub versioned_hash: H256,
    pub data: Vec<u8>,
}

pub struct BlobSidecar {
    blobs: Vec<PreparedBlob>,
}

impl BlobSidecar {
    pub fn new(data: Vec<u8>) -> std::result::Result<Self, Error> {
        let num_blobs = data.len().div_ceil(MAX_BYTES_PER_BLOB);

        if num_blobs > MAX_BLOBS_PER_BLOCK {
            return Err(Error::Other(format!(
                "Data cannot fit into the maximum number of blobs per block: {}",
                MAX_BLOBS_PER_BLOCK
            )));
        }

        let field_elements = Self::generate_field_elements(data);
        let blobs = Self::field_elements_to_blobs(field_elements);
        let prepared_blobs = blobs
            .iter()
            .map(Self::prepare_blob)
            .collect::<Result<_, _>>()?;

        Ok(Self {
            blobs: prepared_blobs,
        })
    }

    pub fn num_blobs(&self) -> usize {
        self.blobs.len()
    }

    pub fn versioned_hashes(&self) -> Vec<H256> {
        self.blobs.iter().map(|blob| blob.versioned_hash).collect()
    }

    // When preparing a blob transaction, we compute the KZG commitment and proof for the blob data.
    // To be able to apply the KZG commitment scheme, the data is treated as a polynomial with the field elements as coefficients.
    // We split it into 31-byte chunks (field elements) padded with a zero byte.
    fn generate_field_elements(data: Vec<u8>) -> Vec<[u8; 32]> {
        data.chunks(31)
            .map(|chunk| {
                let mut fe = [0u8; 32];
                fe[1..1 + chunk.len()].copy_from_slice(chunk);
                fe
            })
            .collect()
    }

    // Generate the right amount of blobs to carry all the field elements.
    fn field_elements_to_blobs(field_elements: Vec<[u8; 32]>) -> Vec<c_kzg::Blob> {
        use itertools::Itertools;

        const ELEMENTS_PER_BLOB: usize = c_kzg::BYTES_PER_BLOB / 32;
        field_elements
            .into_iter()
            .chunks(ELEMENTS_PER_BLOB)
            .into_iter()
            .map(|elements| {
                let mut blob = [0u8; c_kzg::BYTES_PER_BLOB];
                let mut offset = 0;
                for fe in elements {
                    blob[offset..offset + 32].copy_from_slice(&fe);
                    offset += 32;
                }
                blob.into()
            })
            .collect()
    }

    fn prepare_blob(blob: &c_kzg::Blob) -> Result<PreparedBlob, Error> {
        let commitment = Self::kzg_commitment(blob)?;
        let versioned_hash = Self::commitment_to_versioned_hash(&commitment);
        let proof = Self::kzg_proof(blob, &commitment)?;

        Ok(PreparedBlob {
            commitment: commitment.to_vec(),
            proof: proof.to_vec(),
            versioned_hash,
            data: blob.to_vec(),
        })
    }

    fn kzg_commitment(blob: &c_kzg::Blob) -> Result<c_kzg::KzgCommitment, Error> {
        c_kzg::KzgCommitment::blob_to_kzg_commitment(blob, kzg_settings())
            .map_err(|e| Error::Other(e.to_string()))
    }

    fn commitment_to_versioned_hash(commitment: &c_kzg::KzgCommitment) -> H256 {
        const VERSION: u8 = 1;
        let mut res: [u8; 32] = Sha256::digest(commitment.deref()).into();
        res[0] = VERSION;
        H256::from(res)
    }

    fn kzg_proof(
        blob: &c_kzg::Blob,
        commitment: &c_kzg::KzgCommitment,
    ) -> Result<c_kzg::KzgProof, Error> {
        c_kzg::KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), kzg_settings())
            .map_err(|e| Error::Other(e.to_string()))
    }
}

pub struct BlobTransaction {
    pub to: Address,
    pub chain_id: U256,
    pub gas_limit: U256,
    pub nonce: U256,
    pub max_fee_per_gas: U256,
    pub max_priority_fee_per_gas: U256,
    pub max_fee_per_blob_gas: U256,
    pub blob_versioned_hashes: Vec<H256>,
}

pub struct BlobTransactionEncoder {
    tx: BlobTransaction,
    sidecar: BlobSidecar,
}

impl BlobTransactionEncoder {
    pub fn new(tx: BlobTransaction, sidecar: BlobSidecar) -> Self {
        Self { tx, sidecar }
    }

    pub async fn raw_signed_w_sidecar(
        self,
        signer: &impl BlobSigner,
    ) -> crate::error::Result<(H256, Vec<u8>)> {
        let signed_tx_bytes = self.raw_signed(signer).await?;
        let tx_hash = H256(keccak256(&signed_tx_bytes));
        let final_bytes = self.encode_sidecar(signed_tx_bytes);

        Ok((tx_hash, final_bytes))
    }

    fn encode_sidecar(self, payload: Vec<u8>) -> Vec<u8> {
        let blobs_count = self.sidecar.num_blobs();

        let mut stream = RlpStream::new();
        // 4 fields: tx type, blobs, commitments, proofs
        stream.begin_list(4);

        // skip the tx type byte
        stream.append_raw(&payload[1..], 1);

        let mut blob_stream = RlpStream::new_list(blobs_count);
        let mut commitment_stream = RlpStream::new_list(blobs_count);
        let mut proof_stream = RlpStream::new_list(blobs_count);

        for blob in self.sidecar.blobs {
            blob_stream.append(&blob.data);
            commitment_stream.append(&blob.commitment);
            proof_stream.append(&blob.proof);
        }

        stream.append_raw(&blob_stream.out(), 1);
        stream.append_raw(&commitment_stream.out(), 1);
        stream.append_raw(&proof_stream.out(), 1);

        let tx = [&[BLOB_TX_TYPE], stream.as_raw()].concat();

        tx
    }

    async fn raw_signed(&self, signer: &impl BlobSigner) -> crate::error::Result<Vec<u8>> {
        let tx_bytes = self.encode(None);
        let signature = self.compute_signature(tx_bytes, signer).await?;

        Ok(self.encode(Some(signature)))
    }

    async fn compute_signature(
        &self,
        tx_bytes: Vec<u8>,
        signer: &impl BlobSigner,
    ) -> crate::error::Result<Signature> {
        let message_hash = H256::from(keccak256(tx_bytes));

        signer.sign_hash(message_hash).await
    }

    fn encode(&self, signature: Option<Signature>) -> Vec<u8> {
        let tx_bytes = if let Some(signature) = signature {
            self.rlp_signed(signature)
        } else {
            self.rlp()
        };

        [&[BLOB_TX_TYPE], tx_bytes.as_slice()].concat()
    }

    fn rlp(&self) -> Vec<u8> {
        let mut stream = RlpStream::new();
        // 11 fields: common tx fields, unused fields, blob tx fields
        stream.begin_list(11);

        self.append_common_tx_fields(&mut stream);
        Self::append_unused_fields(&mut stream);
        self.append_blob_tx_fields(&mut stream);

        stream.as_raw().to_vec()
    }

    fn rlp_signed(&self, signature: Signature) -> Vec<u8> {
        let mut stream = RlpStream::new();
        // 14 fields: common tx fields, unused fields, blob tx fields, signature
        stream.begin_list(14);

        self.append_common_tx_fields(&mut stream);
        Self::append_unused_fields(&mut stream);
        self.append_blob_tx_fields(&mut stream);

        self.append_signature(&mut stream, signature);

        stream.as_raw().to_vec()
    }

    fn append_common_tx_fields(&self, stream: &mut RlpStream) {
        stream.append(&self.tx.chain_id);
        stream.append(&self.tx.nonce);
        stream.append(&self.tx.max_priority_fee_per_gas);
        stream.append(&self.tx.max_fee_per_gas);
        stream.append(&self.tx.gas_limit);
        stream.append(&self.tx.to);
    }

    fn append_unused_fields(stream: &mut RlpStream) {
        // value, data and access_list
        stream.append_empty_data();
        stream.append_empty_data();
        stream.begin_list(0);
    }

    fn append_blob_tx_fields(&self, stream: &mut RlpStream) {
        stream.append(&self.tx.max_fee_per_blob_gas);
        stream.append_list(&self.tx.blob_versioned_hashes);
    }

    fn append_signature(&self, stream: &mut RlpStream, signature: Signature) {
        stream.append(&signature.v);
        stream.append(&signature.r);
        stream.append(&signature.s);
    }
}
