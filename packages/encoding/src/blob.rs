mod decoder;
mod encoder;
mod header;

pub use decoder::Decoder;
pub use encoder::Encoder;
pub use header::*;

use crate::constants::BYTES_PER_BLOB;

pub type Blob = Box<[u8; BYTES_PER_BLOB]>;

#[cfg(feature = "kzg")]
pub fn generate_sidecar(
    blobs: impl IntoIterator<Item = Blob>,
) -> anyhow::Result<alloy::consensus::BlobTransactionSidecar> {
    let blobs = blobs
        .into_iter()
        .map(|blob| alloy::eips::eip4844::Blob::from(*blob))
        .collect::<Vec<_>>();
    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());
    let settings = alloy::consensus::EnvKzgSettings::default();

    for blob in &blobs {
        // SAFETY: same size
        let blob =
            unsafe { core::mem::transmute::<&alloy::eips::eip4844::Blob, &c_kzg::Blob>(blob) };
        let commitment = c_kzg::KzgCommitment::blob_to_kzg_commitment(blob, settings.get())?;
        let proof =
            c_kzg::KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), settings.get())?;

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

#[cfg(feature = "native-kzg")]
pub mod native_kzg {
    use super::*;
    use rust_eth_kzg::constants::{CELLS_PER_EXT_BLOB, NUM_PROOFS};
    use rust_eth_kzg::{Cell, KZGCommitment, KZGProof};
    use rust_eth_kzg::{DASContext, TrustedSetup, UsePrecomp};

    /// The generated sidecar from a set of blobs
    pub struct BlobSidecar {
        pub commitments: Vec<KZGCommitment>,
        pub proofs: Vec<[KZGProof; NUM_PROOFS]>,
        pub blob_cells: Vec<[Cell; CELLS_PER_EXT_BLOB]>,
        pub blobs: Vec<Blob>,
    }

    #[cfg(feature = "kzg")]
    impl From<BlobSidecar> for alloy::consensus::BlobTransactionSidecar {
        fn from(sidecar: BlobSidecar) -> Self {
            let BlobSidecar {
                blobs,
                proofs,
                commitments,
                ..
            } = sidecar;

            let alloy_blobs = blobs
                .into_iter()
                .map(|blob| alloy::eips::eip4844::Blob::from(*blob))
                .collect::<Vec<_>>();

            let alloy_proofs = proofs
                .into_iter()
                .flat_map(|proof_array| {
                    proof_array.into_iter().map(|proof| {
                        // SAFETY: KZGProof and Bytes48 have identical memory layouts
                        unsafe {
                            core::mem::transmute::<KZGProof, alloy::eips::eip4844::Bytes48>(proof)
                        }
                    })
                })
                .collect::<Vec<_>>();

            let alloy_commitments = commitments
                .into_iter()
                .map(|commitment| {
                    // SAFETY: KZGCommitment and Bytes48 have identical memory layouts
                    unsafe {
                        core::mem::transmute::<KZGCommitment, alloy::eips::eip4844::Bytes48>(commitment)
                    }
                })
                .collect::<Vec<_>>();

            alloy::consensus::BlobTransactionSidecar::new(
                alloy_blobs,
                alloy_commitments,
                alloy_proofs,
            )
        }
    }

    /// Generate the sidecar from a set of blobs
    pub fn generate_sidecar(blobs: impl IntoIterator<Item = Blob>) -> anyhow::Result<BlobSidecar> {
        let trusted_setup = TrustedSetup::default();
        let ctx = DASContext::new(&trusted_setup, UsePrecomp::Yes { width: 8 });

        let blobs = blobs.into_iter().collect::<Vec<_>>();

        let mut commitments = Vec::with_capacity(blobs.len());
        let mut proofs = Vec::with_capacity(blobs.len());
        let mut blob_cells = Vec::with_capacity(blobs.len());

        for blob in &blobs {
            let (cell, proof) = ctx
                .compute_cells_and_kzg_proofs(blob)
                .map_err(|e| anyhow::anyhow!("Failed to compute kzg proofs: {e:?}"))?;

            let commitment = ctx
                .blob_to_kzg_commitment(blob)
                .map_err(|e| anyhow::anyhow!("Failed to compute kzg commitment: {e:?}"))?;

            proofs.push(proof);
            blob_cells.push(cell);
            commitments.push(commitment);
        }

        Ok(BlobSidecar {
            commitments,
            proofs,
            blob_cells,
            blobs,
        })
    }

    /// Verify the contents of the sidecar
    pub fn verify_sidecar(sidecar: BlobSidecar) -> anyhow::Result<()> {
        let BlobSidecar {
            commitments,
            proofs,
            blob_cells,
            ..
        } = sidecar;

        let trusted_setup = TrustedSetup::default();
        let ctx = DASContext::new(&trusted_setup, UsePrecomp::Yes { width: 8 });

        let mut cell_commitments = Vec::with_capacity(commitments.len());
        let mut cell_indices = Vec::with_capacity(commitments.len());
        let mut cells = Vec::with_capacity(blob_cells.len());
        let mut cell_proofs = Vec::with_capacity(proofs.len());

        for (row_index, blob_cell) in blob_cells.iter().enumerate() {
            for (cell_index, cell) in blob_cell.iter().enumerate() {
                cell_commitments.push(&commitments[row_index]);
                cell_indices.push(cell_index as u64);
                cells.push(cell.as_ref());
                cell_proofs.push(&proofs[row_index][cell_index]);
            }
        }

        ctx.verify_cell_kzg_proof_batch(cell_commitments, cell_indices, cells, cell_proofs)
            .map_err(|e| anyhow::anyhow!("Couldn't verify batch of proofs: {e:?}"))?;

        Ok(())
    }
}
