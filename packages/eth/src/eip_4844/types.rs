use std::{ffi::CString, ops::Deref};

use ethers::{
    core::k256::sha2::{Digest, Sha256},
    types::{Address, Signature, H256, U256},
    utils::keccak256,
};
use lazy_static::lazy_static;
use rlp::RlpStream;

lazy_static! {
    static ref KZG_SETTINGS: c_kzg::KzgSettings = c_kzg::KzgSettings::load_trusted_setup_file(
        &CString::new("packages/eth/src/eip_4844/trusted_setup.txt").expect("C string"),
    )
    .unwrap();
}

const BLOB_TX_TYPE: u8 = 0x03;
const VERSIONED_HASH_VERSION_KZG: u8 = 1;
const MAX_BLOBS_PER_BLOCK: usize = 6;
pub const MAX_BYTES_PER_BLOB: usize = c_kzg::BYTES_PER_BLOB;

pub trait BlobSigner {
    fn sign_hash(&self, hash: H256) -> std::result::Result<Signature, String>;
}

impl BlobSigner for ethers::signers::LocalWallet {
    fn sign_hash(&self, hash: H256) -> std::result::Result<Signature, String> {
        self.sign_hash(hash).map_err(|e| e.to_string())
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
    pub fn new(data: Vec<u8>) -> std::result::Result<Self, String> {
        let num_blobs = data.len().div_ceil(MAX_BYTES_PER_BLOB);

        if num_blobs > MAX_BLOBS_PER_BLOCK {
            return Err(format!(
                "Data cannot fit into the maximum number of blobs per block: {}",
                MAX_BLOBS_PER_BLOCK
            ));
        }

        let field_elements = Self::partition_data(data);
        let blobs = Self::field_elements_to_blobs(field_elements);
        let prepared_blobs = blobs.iter().map(Self::prepare_blob).collect();

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

    fn partition_data(data: Vec<u8>) -> Vec<[u8; 32]> {
        let capacity = data.len().div_ceil(31);
        let mut field_elements = Vec::with_capacity(capacity);
        for chunk in data.chunks(31) {
            let mut fe = [0u8; 32];
            fe[1..1 + chunk.len()].copy_from_slice(chunk);
            field_elements.push(fe);
        }
        field_elements
    }

    fn field_elements_to_blobs(field_elements: Vec<[u8; 32]>) -> Vec<c_kzg::Blob> {
        let mut blobs = Vec::new();
        let mut current_blob = [0u8; c_kzg::BYTES_PER_BLOB];
        let mut offset = 0;

        for fe in field_elements {
            if offset + 32 > c_kzg::BYTES_PER_BLOB {
                blobs.push(current_blob.into());
                current_blob = [0u8; c_kzg::BYTES_PER_BLOB];
                offset = 0;
            }
            current_blob[offset..offset + 32].copy_from_slice(&fe);
            offset += 32;
        }

        if offset > 0 {
            blobs.push(current_blob.into());
        }

        blobs
    }

    fn prepare_blob(blob: &c_kzg::Blob) -> PreparedBlob {
        let commitment = Self::kzg_commitment(blob);
        let versioned_hash = Self::commitment_to_versioned_hash(&commitment);
        let proof = Self::kzg_proof(blob, &commitment);

        PreparedBlob {
            commitment: commitment.to_vec(),
            proof: proof.to_vec(),
            versioned_hash,
            data: blob.to_vec(),
        }
    }

    fn kzg_commitment(blob: &c_kzg::Blob) -> c_kzg::KzgCommitment {
        c_kzg::KzgCommitment::blob_to_kzg_commitment(blob, &KZG_SETTINGS).unwrap()
    }

    fn commitment_to_versioned_hash(commitment: &c_kzg::KzgCommitment) -> H256 {
        let mut res: [u8; 32] = Sha256::digest(commitment.deref()).into();
        res[0] = VERSIONED_HASH_VERSION_KZG;
        H256::from(res)
    }

    fn kzg_proof(blob: &c_kzg::Blob, commitment: &c_kzg::KzgCommitment) -> c_kzg::KzgProof {
        c_kzg::KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), &KZG_SETTINGS)
            .unwrap()
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

    pub fn raw_signed_w_sidecar(self, signer: &impl BlobSigner) -> (H256, Vec<u8>) {
        let signed_tx_bytes = self.raw_signed(signer);
        let tx_hash = H256(keccak256(&signed_tx_bytes));
        let final_bytes = self.encode_sidecar(signed_tx_bytes);

        (tx_hash, final_bytes)
    }

    fn encode_sidecar(self, payload: Vec<u8>) -> Vec<u8> {
        let blobs_count = self.sidecar.num_blobs();

        let mut stream = RlpStream::new();
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

    fn raw_signed(&self, signer: &impl BlobSigner) -> Vec<u8> {
        let tx_bytes = self.encode(None);
        let signature = self.compute_signature(tx_bytes, signer);

        self.encode(Some(signature))
    }

    fn compute_signature(&self, tx_bytes: Vec<u8>, signer: &impl BlobSigner) -> Signature {
        let message_hash = H256::from(keccak256(tx_bytes));

        signer
            .sign_hash(message_hash)
            .expect("signing should not fail")
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
        stream.begin_list(11);

        self.append_common_tx_fields(&mut stream);
        Self::append_unused_fields(&mut stream);
        self.append_blob_tx_fields(&mut stream);

        stream.as_raw().to_vec()
    }

    fn rlp_signed(&self, signature: Signature) -> Vec<u8> {
        let mut stream = RlpStream::new();
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
