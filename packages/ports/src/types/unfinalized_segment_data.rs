use super::ValidatedRange;

#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionDataSlice {
    pub bytes: Vec<u8>,
    pub location_in_segment: ValidatedRange,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnfinalizedSubmissionData {
    pub submission_id: u32,
    pub data_slice: SubmissionDataSlice,
}
