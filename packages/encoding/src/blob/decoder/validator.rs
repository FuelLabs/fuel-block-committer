use std::collections::{BTreeSet, HashSet};

use anyhow::bail;
use bitvec::{order::Msb0, slice::BitSlice};
use itertools::Itertools;

use crate::blob::{Blob, Header};

use super::super::HeaderV1;

struct BlobWithHeader<'a> {
    header: HeaderV1,
    data: &'a BitSlice<u8, Msb0>,
}

pub struct BlobValidator<'a> {
    pub(crate) blobs: Vec<BlobWithHeader<'a>>,
}

impl<'a> BlobValidator<'a> {
    pub(crate) fn for_blobs(blobs: &'a [Blob]) -> anyhow::Result<Self> {
        let blobs  = blobs
                    .iter()
                    .map(|blob| {
                        let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

                        let (header, _) = Header::decode(&buffer[2..])?;
                        let Header::V1(header) = header;
                        let max_bits_per_blob = 4096 * 256;
                        if header.num_bits > max_bits_per_blob {
                            bail!(
                                "num_bits of blob (bundle_id: {}, idx: {}) is greater than the maximum allowed value of {max_bits_per_blob}", header.bundle_id, header.idx
                            );
                        }

                        let data_end = header.num_bits as usize;

                        let data = &buffer[..data_end];

                        Ok(BlobWithHeader {
                                            header,
                                            data,
                                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self { blobs })
    }

    pub(crate) fn validated_blobs(mut self) -> anyhow::Result<Vec<&'a BitSlice<u8, Msb0>>> {
        self.blobs.sort_by_key(|blob| blob.header.idx);

        if self.blobs.is_empty() {
            bail!("No blobs to decode");
        }

        let highest_idx = self.blobs.last().expect("At least one blob").header.idx;

        self.all_blobs_belong_to_same_bundle()?;
        self.last_blob_correctly_marked(highest_idx)?;
        self.has_no_missing_idx(highest_idx)?;
        self.no_duplicates_in_idx()?;

        Ok(self.blobs.into_iter().map(|blob| blob.data).collect())
    }

    pub(crate) fn no_duplicates_in_idx(&self) -> anyhow::Result<()> {
        let duplicates = self
            .blobs
            .iter()
            .duplicates_by(|blob| blob.header.idx)
            .collect_vec();

        if !duplicates.is_empty() {
            let msg = duplicates
                .iter()
                .map(|blob| blob.header.idx.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("found duplicate blob idxs: {msg}",);
        }

        Ok(())
    }

    pub(crate) fn has_no_missing_idx(&self, highest_idx: u32) -> anyhow::Result<()> {
        let present_idxs: HashSet<u32> = self.blobs.iter().map(|blob| blob.header.idx).collect();

        let missing_idxs: Vec<u32> = (0..=highest_idx)
            .filter(|idx| !present_idxs.contains(idx))
            .collect();

        if !missing_idxs.is_empty() {
            let msg = missing_idxs
                .iter()
                .map(|idx| idx.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("missing blobs with indexes: {msg}");
        }
        Ok(())
    }

    pub(crate) fn last_blob_correctly_marked(&self, highest_idx: u32) -> anyhow::Result<()> {
        let blobs_marked_as_last: Vec<_> = self
            .blobs
            .iter()
            .filter(|blob| blob.header.is_last)
            .collect();

        if blobs_marked_as_last.is_empty() {
            bail!("no blob is marked as last");
        }

        if blobs_marked_as_last.len() > 1 {
            let msg = blobs_marked_as_last
                .iter()
                .map(|blob| blob.header.idx.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("multiple blobs marked as being the last blob. blobs with indexes: {msg}");
        }

        if blobs_marked_as_last[0].header.idx != highest_idx {
            bail!(
                "blob with highest index is {}, but the blob marked as last has index {}",
                highest_idx,
                blobs_marked_as_last[0].header.idx
            );
        }

        Ok(())
    }

    pub(crate) fn all_blobs_belong_to_same_bundle(&self) -> anyhow::Result<()> {
        // BTreeSet so that we can get a sorted list of bundle ids, easier to test
        let unique_bundle_ids: BTreeSet<_> = self
            .blobs
            .iter()
            .map(|blob| blob.header.bundle_id)
            .collect();

        if unique_bundle_ids.len() != 1 {
            bail!(
                "All blobs must have the same bundle id, got {:?}",
                unique_bundle_ids
            );
        }

        Ok(())
    }
}
