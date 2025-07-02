mod decoder;
mod encoder;
mod header;

pub use decoder::Decoder;
pub use encoder::Encoder;
pub use header::*;

use crate::constants::BYTES_PER_BLOB;

pub type Blob = Box<[u8; BYTES_PER_BLOB]>;

// #[cfg(feature = "kzg")]
pub fn generate_sidecar(
    blobs: impl IntoIterator<Item = Blob>,
) -> anyhow::Result<alloy::consensus::BlobTransactionSidecar> {
    let blobs = blobs
        .into_iter()
        .map(|blob| alloy::eips::eip4844::Blob::from(*blob))
        .collect::<Vec<_>>();
    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());
    let env_settings = alloy::consensus::EnvKzgSettings::default();
    let settings = env_settings.get();

    for blob in &blobs {
        // SAFETY: same size
        let blob =
            unsafe { core::mem::transmute::<&alloy::eips::eip4844::Blob, &c_kzg::Blob>(blob) };
        let commitment = settings.blob_to_kzg_commitment(&blob)?;
        let proof = settings.compute_blob_kzg_proof(blob, &commitment.to_bytes())?;

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

    Ok(alloy::consensus::BlobTransactionSidecar::new(
        blobs,
        commitments,
        proofs,
    ))
}
