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

#[cfg(feature = "native-kzg-verify")]
pub mod native_kzg_verify {
    use kzg_rs::{
        kzg_proof::{
            safe_g1_affine_from_bytes,
            G1Projective, G2Projective, Scalar,
        },
        pairings_verify, KzgSettings,
    };

    #[cfg(feature = "kzg")]
    use kzg_rs::kzg_proof::{compute_challenge, evaluate_polynomial_in_evaluation_form};
    #[cfg(feature = "kzg")]
    use kzg_rs::kzg_proof::{Digest, Sha256};


    #[cfg(feature = "kzg")]
    pub fn verify_kzg_sidecar(
        kzg_settings: &KzgSettings,
        sidecar: alloy::consensus::BlobTransactionSidecar,
    ) -> anyhow::Result<bool> {
        use kzg_rs::KzgProof;

        let VerifierSidecar {
            blobs,
            proofs,
            commitments,
        } = transmute_sidecar(sidecar);

        let is_verified =
            KzgProof::verify_blob_kzg_proof_batch(blobs, commitments, proofs, &kzg_settings)
                .map_err(|e| anyhow::anyhow!("couldn't verify proof: {e:?}"))?;

        Ok(is_verified)
    }

    #[cfg(feature = "kzg")]
    pub struct VerifierSidecar {
        blobs: Vec<kzg_rs::Blob>,
        proofs: Vec<kzg_rs::Bytes48>,
        commitments: Vec<kzg_rs::Bytes48>,
    }

    #[cfg(feature = "kzg")]
    pub fn transmute_sidecar(sidecar: alloy::consensus::BlobTransactionSidecar) -> VerifierSidecar {
        let mut blobs = Vec::with_capacity(sidecar.blobs.len());
        let mut commitments = Vec::with_capacity(sidecar.commitments.len());
        let mut proofs = Vec::with_capacity(sidecar.proofs.len());

        for blob in sidecar.blobs {
            // SAFETY: same size, 131072 bytes
            unsafe {
                blobs.push(core::mem::transmute::<
                    alloy::eips::eip4844::Blob,
                    kzg_rs::Blob,
                >(blob));
            }
        }

        for commitment in sidecar.commitments {
            // SAFETY: same size, 48 bytes
            unsafe {
                commitments.push(core::mem::transmute::<
                    alloy::eips::eip4844::Bytes48,
                    kzg_rs::Bytes48,
                >(commitment));
            }
        }

        for proof in sidecar.proofs {
            // SAFETY: same size, 48 bytes
            unsafe {
                proofs.push(core::mem::transmute::<
                    alloy::eips::eip4844::Bytes48,
                    kzg_rs::Bytes48,
                >(proof));
            }
        }

        VerifierSidecar {
            blobs,
            commitments,
            proofs,
        }
    }

    #[derive(Debug)]
    pub struct PrecompileInput {
        pub commitment: kzg_rs::Bytes48,
        pub proof: kzg_rs::Bytes48,
        pub z: Scalar,
        pub y: Scalar,
        pub versioned_hash: [u8; 32],
    }

    #[cfg(feature = "kzg")]
    pub fn commitment_to_versioned_hash(commitment: &kzg_rs::Bytes48) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(commitment.as_slice());
        let hashed_commitment = hasher.finalize();

        static KZG_VERSIONED_HASH: u8 = 0x01;

        let mut result = [0u8; 32];
        result.copy_from_slice(hashed_commitment.as_slice());
        result[0] = KZG_VERSIONED_HASH;
        result
    }

    #[cfg(feature = "kzg")]
    pub fn generate_precompile_inputs(
        kzg_settings: &KzgSettings,
        sidecar: alloy::eips::eip4844::BlobTransactionSidecar,
    ) -> anyhow::Result<Vec<PrecompileInput>> {
        let mut precompile_inputs = Vec::with_capacity(sidecar.blobs.len());

        let sidecar = transmute_sidecar(sidecar);

        for i in 0..sidecar.blobs.len() {
            let commitment = sidecar.commitments[i].clone();
            let commitment_on_curve = safe_g1_affine_from_bytes(&commitment)
                .map_err(|e| anyhow::anyhow!("cant cast to g1 affine: {e:?}"))?;
            let proof = sidecar.proofs[i].clone();
            let versioned_hash = commitment_to_versioned_hash(&commitment);

            let polynomial = sidecar.blobs[i]
                .as_polynomial()
                .map_err(|e| anyhow::anyhow!("cant convert blob to polynomial: {e:?}"))?;
            let z = compute_challenge(&sidecar.blobs[i], &commitment_on_curve)
                .map_err(|e| anyhow::anyhow!("cant compute challenge: {e:?}"))?;
            let y = evaluate_polynomial_in_evaluation_form(polynomial, z, &kzg_settings)
                .map_err(|e| anyhow::anyhow!("cant evaluate polynomial: {e:?}"))?;
            // recompute y,z, polynomial inside the zkvm, verify against PrecompileInput
            // lets ignore commitment for now

            precompile_inputs.push(PrecompileInput {
                commitment,
                proof,
                versioned_hash,
                z,
                y,
            });
        }

        Ok(precompile_inputs)
    }

    pub fn verify_precompile_inputs(
        kzg_settings: &KzgSettings,
        precompile_inputs: Vec<PrecompileInput>,
    ) -> anyhow::Result<bool> {
        for precompile_input in precompile_inputs {
            let commitment_on_curve = safe_g1_affine_from_bytes(&precompile_input.commitment)
                .map_err(|e| anyhow::anyhow!("cant cast to g1 affine: {e:?}"))?;

            // investigate if we need to do the subtractio
            // no idea if precompile uses multi-miller loop pairing or not
            let x = G2Projective::generator() * precompile_input.z;
            let x_minus_z = kzg_settings.g2_points[1] - x;

            let y = G1Projective::generator() * precompile_input.y;
            let p_minus_y = commitment_on_curve - y;

            let res = pairings_verify(
                p_minus_y.into(),
                G2Projective::generator().into(),
                safe_g1_affine_from_bytes(&precompile_input.proof)
                    .map_err(|e| anyhow::anyhow!(e))?,
                x_minus_z.into(),
            );

            match res {
                true => continue,
                false => return Ok(false),
            }
        }

        Ok(true)
    }

    #[cfg(test)]
    mod tests {
        use crate::blob::{generate_sidecar, native_kzg_verify::PrecompileInput};

        use super::{generate_precompile_inputs, verify_kzg_sidecar, verify_precompile_inputs};

        #[test]
        fn sidecar_generated_by_c_kzg_is_verified_by_kzg_rs() {
            let kzg_settings = kzg_rs::KzgSettings::load_trusted_setup_file().unwrap();

            let blob_count = 3;
            let mut blobs = Vec::with_capacity(blob_count);

            for _ in 0..blob_count {
                blobs.push(Box::new([1u8; 131072]));
            }

            let sidecar = generate_sidecar(blobs).unwrap();

            let sidecar_verified = verify_kzg_sidecar(&kzg_settings, sidecar).unwrap();

            assert!(sidecar_verified);
        }

        #[test]
        fn sidecar_generated_by_c_kzg_can_produce_verifiable_precompile_input() {
            let kzg_settings = kzg_rs::KzgSettings::load_trusted_setup_file().unwrap();

            let blob_count = 3;
            let mut blobs = Vec::with_capacity(blob_count);

            for _ in 0..blob_count {
                blobs.push(Box::new([1u8; 131072]));
            }

            let sidecar = generate_sidecar(blobs).unwrap();

            let precompile_inputs = generate_precompile_inputs(&kzg_settings, sidecar).unwrap();

            let res = verify_precompile_inputs(&kzg_settings, precompile_inputs).unwrap();

            assert!(res);
        }

        #[test]
        fn size_of_precompile_input_matches_ethereum_precompile_input_size() {
            assert_eq!(core::mem::size_of::<PrecompileInput>(), 192);
        }
    }
}
