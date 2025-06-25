use services::types::{DateTime, DispersalStatus, Utc};

#[derive(sqlx::FromRow)]
pub struct EigenDASubmission {
    pub id: i32,
    pub request_id: Vec<u8>,
    pub created_at: Option<DateTime<Utc>>,
    pub status: i16,
}

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err($crate::error::Error::Conversion(format!($msg, $($args),*)))
    };
}

impl EigenDASubmission {
    pub fn parse_status(&self) -> Result<DispersalStatus, crate::error::Error> {
        match self.status {
            0 => Ok(DispersalStatus::Processing),
            1 => Ok(DispersalStatus::Finalized),
            2 => Ok(DispersalStatus::Failed),
            3 => Ok(DispersalStatus::Confirmed),
            _ => {
                bail!(
                    "EigenDASubmission(id={}) has invalid status {}",
                    self.id,
                    self.status
                )
            }
        }
    }
}

impl TryFrom<EigenDASubmission> for services::types::EigenDASubmission {
    type Error = crate::error::Error;

    fn try_from(value: EigenDASubmission) -> Result<Self, Self::Error> {
        let status = value.parse_status()?;

        let id = value.id.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `id` to u64. Got: {} from db",
                value.id
            ))
        })?;

        Ok(Self {
            id: Some(id),
            request_id: value.request_id,
            status,
            created_at: value.created_at,
        })
    }
}

pub enum SubmissionStatus {
    Processing,
    Confirmed,
    Finalized,
    Failed,
}

impl From<DispersalStatus> for SubmissionStatus {
    fn from(status: DispersalStatus) -> Self {
        match status {
            DispersalStatus::Processing => SubmissionStatus::Processing,
            DispersalStatus::Confirmed => SubmissionStatus::Confirmed,
            DispersalStatus::Finalized => SubmissionStatus::Finalized,
            DispersalStatus::Failed | DispersalStatus::Other(_) => SubmissionStatus::Failed,
        }
    }
}

impl From<SubmissionStatus> for i16 {
    fn from(status: SubmissionStatus) -> Self {
        match status {
            SubmissionStatus::Processing => 0,
            SubmissionStatus::Finalized => 1,
            SubmissionStatus::Failed => 2,
            SubmissionStatus::Confirmed => 3,
        }
    }
}
