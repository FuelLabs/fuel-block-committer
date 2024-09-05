use super::ValidatedRange;

#[derive(Debug, Clone)]
pub struct SegmentDataSlice {
    pub bytes: Vec<u8>,
    pub location_in_segment: ValidatedRange,
}

#[derive(Debug, Clone)]
pub struct UnfinalizedSegmentData {
    pub submission_id: u32,
    pub data_slice: SegmentDataSlice,
}
