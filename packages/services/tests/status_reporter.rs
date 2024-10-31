use std::sync::Arc;

use clock::TestClock;
use ports::{
    clock::Clock,
    storage::Storage,
    types::{BlockSubmission, BlockSubmissionTx},
};
use rand::Rng;
use services::status_reporter::{Status, StatusReport, StatusReporter};
use storage::PostgresProcess;

#[tokio::test]
async fn status_depends_on_last_submission() {
    let process = PostgresProcess::shared().await.unwrap();
    let test = |submission_status, expected_app_status| {
        let process = Arc::clone(&process);
        async move {
            // given
            let mut rng = rand::thread_rng();
            let db = process.create_random_db().await.unwrap();

            if let Some(is_completed) = submission_status {
                let latest_submission = BlockSubmission {
                    completed: is_completed,
                    ..rng.gen()
                };
                db.record_block_submission(
                    BlockSubmissionTx::default(),
                    latest_submission,
                    TestClock::default().now(),
                )
                .await
                .unwrap();
            }

            let status_reporter = StatusReporter::new(db);

            // when
            let status = status_reporter.current_status().await.unwrap();

            // then
            assert_eq!(
                status,
                StatusReport {
                    status: expected_app_status
                }
            );
        }
    };

    // has an entry, not completed
    test(Some(false), Status::Committing).await;
    // has an entry, completed
    test(Some(true), Status::Idle).await;
    // has no entry
    test(None, Status::Idle).await;
}
