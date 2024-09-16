use ports::{
    clock::Clock,
    types::{DateTime, Utc},
};

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(feature = "test-helpers")]
mod test_helpers {
    use std::{
        sync::{atomic::AtomicI64, Arc},
        time::Duration,
    };

    use ports::{
        clock::Clock,
        types::{DateTime, Utc},
    };

    #[derive(Default, Clone)]
    pub struct TestClock {
        epoch_millis: Arc<AtomicI64>,
    }

    impl TestClock {
        pub fn advance_time(&self, adv: Duration) {
            let new_time = self.now() + adv;
            self.epoch_millis.store(
                new_time.timestamp_millis(),
                std::sync::atomic::Ordering::Relaxed,
            )
        }
        pub fn set_time(&self, new_time: DateTime<Utc>) {
            self.epoch_millis.store(
                new_time.timestamp_millis(),
                std::sync::atomic::Ordering::Relaxed,
            )
        }
    }

    #[async_trait::async_trait]
    impl Clock for TestClock {
        fn now(&self) -> ports::types::DateTime<ports::types::Utc> {
            DateTime::<Utc>::from_timestamp_millis(
                self.epoch_millis.load(std::sync::atomic::Ordering::Relaxed),
            )
            .expect("DateTime<Utc> to be in range")
        }
    }
}

#[cfg(feature = "test-helpers")]
pub use test_helpers::TestClock;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ports::clock::Clock;

    use crate::TestClock;

    #[tokio::test]
    async fn can_advance_clock() {
        // given
        let test_clock = TestClock::default();
        let starting_time = test_clock.now();
        let adv = Duration::from_secs(1);

        // when
        test_clock.advance_time(adv).await;

        // then
        let new_time = starting_time + adv;
        assert_eq!(test_clock.now(), new_time);
    }
}
