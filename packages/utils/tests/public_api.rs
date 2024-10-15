#[cfg(test)]
mod test {
    pub(crate) fn generate_sidecar(
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
            let proof =
                KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), settings.get())?;

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

    use alloy::{
        consensus::{BlobTransactionSidecar, EnvKzgSettings},
        eips::eip4844::USABLE_BITS_PER_FIELD_ELEMENT,
    };
    use c_kzg::{KzgCommitment, KzgProof};
    use itertools::Itertools;
    use rand::{rngs::SmallRng, seq::SliceRandom, RngCore, SeedableRng};
    use test_case::test_case;
    use utils::{decoder::NewDecoder, encoder::NewEncoder, Blob, BlobHeader, BlobHeaderV1};

    #[test_case(1,  1; "one blob")]
    #[test_case(130037,  1; "one blob limit")]
    #[test_case(130038,  2; "two blobs")]
    #[test_case(130037 * 2,  2; "two blobs limit")]
    #[test_case(130037 * 2  + 1,  3; "three blobs")]
    fn gas_usage_for_data_storage(num_bytes: usize, num_blobs: usize) {
        // given
        let encoder = NewEncoder {};

        // when
        let usage = encoder.blobs_needed_to_encode(num_bytes);

        // then
        assert_eq!(usage, num_blobs);
    }

    #[test_case(1)]
    #[test_case(50)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn can_generate_proofs_and_committments_for_encoded_blobs(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);

        // when
        let blobs = encoder.encode(&data, 0).unwrap();
        println!("{:?}", &blobs[0][0..8]);

        // then
        for blob in blobs {
            let sidecar = generate_sidecar(vec![blob]).unwrap();
            let versioned_hashes = sidecar.versioned_hashes().collect_vec();
            sidecar
                .validate(&versioned_hashes, EnvKzgSettings::default().get())
                .unwrap();
        }
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn blobs_can_be_decoded_when_in_order(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);
        let blobs = encoder.encode(&data, 0).unwrap();

        let decoder = NewDecoder {};

        // when
        let decoded_data = decoder.decode(&blobs).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn blobs_can_be_decoded_even_if_shuffled_around(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut rng = SmallRng::from_seed([0; 32]);
        let data = {
            let mut data = vec![0; byte_num];
            rng.fill_bytes(&mut data[..]);
            data
        };

        let blobs = {
            let mut blobs = encoder.encode(&data, 0).unwrap();
            blobs.shuffle(&mut rng);
            blobs
        };

        let decoder = NewDecoder {};

        // when
        let decoded_data = decoder.decode(&blobs).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test_case(100, 0; "id 0")]
    #[test_case(100, 5; "normal case")]
    #[test_case(100, u32::MAX; "max id")]
    fn roundtrip_header_encoding(num_bytes: usize, bundle_id: u32) {
        // given
        let blob = {
            let encoder = NewEncoder {};
            encoder
                .encode(&vec![0; num_bytes], bundle_id)
                .unwrap()
                .pop()
                .unwrap()
        };

        eprintln!("{:?}", &blob[0..11]);

        let decoder = NewDecoder {};

        // when
        let header = decoder.read_header(&blob).unwrap();

        // then
        let lost_to_fe = 2 * (num_bytes * 8).div_ceil(256);
        assert_eq!(
            header,
            BlobHeader::V1(BlobHeaderV1 {
                bundle_id,
                num_bits: (num_bytes * 8 + BlobHeader::V1_SIZE_BITS + lost_to_fe) as u32,
                is_last: true,
                idx: 0
            })
        );
    }
}
