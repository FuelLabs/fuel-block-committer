use std::time::Duration;

pub enum DALayerError {
    Eroor,
}

pub enum SubmissionStatus {
    Pending,
    Completed,
    Finalized,
    Failed,
}

pub trait DALayer: Send + Sync {
    fn post_data(
        &self,
        data: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, DALayerError>> + Send;
    fn check_status(
        &self,
        submission_id: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<SubmissionStatus, DALayerError>> + Send;
    fn wait_for_finalization(
        &self,
        request_id: Vec<u8>,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<SubmissionStatus, DALayerError>> + Send;
}
