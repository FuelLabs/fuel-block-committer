use alloy::{consensus::EnvKzgSettings, eips::eip4844::BYTES_PER_BLOB};
pub use alloy::{
    consensus::{Blob as AlloyBlob, BlobTransactionSidecar},
    eips::eip4844::Bytes48,
};
use c_kzg::{KzgCommitment, KzgProof};

pub mod decoder;
pub mod encoder;
pub mod header;

pub type Blob = Box<[u8; BYTES_PER_BLOB]>;

pub fn generate_sidecar(
    blobs: impl IntoIterator<Item = Blob>,
) -> anyhow::Result<BlobTransactionSidecar> {
    let blobs = blobs
        .into_iter()
        .map(|blob| alloy::eips::eip4844::Blob::from(*blob))
        .collect::<Vec<_>>();
    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());
    let settings = EnvKzgSettings::default();

    for blob in &blobs {
        // SAFETY: same size
        let blob =
            unsafe { core::mem::transmute::<&alloy::eips::eip4844::Blob, &c_kzg::Blob>(blob) };
        let commitment = KzgCommitment::blob_to_kzg_commitment(blob, settings.get())?;
        let proof = KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), settings.get())?;

        // SAFETY: same size
        unsafe {
            commitments.push(core::mem::transmute::<
                c_kzg::Bytes48,
                alloy::eips::eip4844::Bytes48,
            >(commitment.to_bytes()));
            proofs.push(core::mem::transmute::<
                c_kzg::Bytes48,
                alloy::eips::eip4844::Bytes48,
            >(proof.to_bytes()));
        }
    }

    Ok(BlobTransactionSidecar::new(blobs, commitments, proofs))
}
