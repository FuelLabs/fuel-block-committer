use ports::types::{SubmissionDataSlice, ValidatedRange};
use sqlx::FromRow;

#[derive(FromRow, Debug)]
pub(crate) struct UnfinalizedSegmentData {
    pub submission_id: i32,
    // https://docs.rs/sqlx/latest/sqlx/macro.query.html#nullability-output-columns
    pub segment_data: Option<Vec<u8>>,
    pub uncommitted_start: Option<i32>,
    pub uncommitted_end: Option<i32>,
}

impl TryFrom<UnfinalizedSegmentData> for ports::types::UnfinalizedSubmissionData {
    type Error = crate::error::Error;

    fn try_from(value: UnfinalizedSegmentData) -> Result<Self, Self::Error> {
        let submission_id = value.submission_id.try_into().map_err(|_| {
            crate::error::Error::Conversion(format!(
                "db submission id ({}) could not be converted into a u32",
                value.submission_id
            ))
        })?;

        let bytes = value.segment_data.ok_or_else(|| {
            crate::error::Error::Conversion(
                "segment data was not found in the database. this is a bug".to_string(),
            )
        })?;

        let (start, end) = value
            .uncommitted_start
            .zip(value.uncommitted_end)
            .ok_or_else(|| {
                crate::error::Error::Conversion(
                    "uncommitted start and end were not found in the database. this is a bug"
                        .to_string(),
                )
            })?;

        let start: u32 = start.try_into().map_err(|_| {
            crate::error::Error::Conversion(format!(
                "db uncommitted start ({}) could not be converted into a u32",
                start
            ))
        })?;

        let end: u32 = end.try_into().map_err(|_| {
            crate::error::Error::Conversion(format!(
                "db uncommitted end ({}) could not be converted into a u32",
                end
            ))
        })?;

        let range = (start..end)
            .try_into()
            .map_err(|e| crate::error::Error::Conversion(format!("{e}")))?;

        let data_slice = SubmissionDataSlice {
            bytes,
            location_in_segment: range,
        };

        Ok(Self {
            submission_id,
            data_slice,
        })
    }
}
