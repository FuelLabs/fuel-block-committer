use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};

use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
};
use tracing::info;

#[derive(Debug, Clone, Copy)]
pub struct Throughput {
    pub bytes_per_sec: NonZeroU32,
    pub max_burst: NonZeroU32,
    pub calls_per_sec: NonZeroU32,
}

#[derive(Debug, Clone)]
pub struct Throttler {
    // Limits the number of bytes that can be posted per second.
    throughput_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
    // Limits the posting frequency to one request per second.
    post_frequency_limiter: Arc<RateLimiter<NotKeyed, InMemoryState, DefaultClock>>,
}

impl Throttler {
    pub fn new(throughput: Throughput) -> Self {
        let throughput_quota =
            Quota::per_second(throughput.bytes_per_sec).allow_burst(throughput.max_burst);
        let post_quota = Quota::per_second(throughput.calls_per_sec);

        Self {
            throughput_limiter: Arc::new(RateLimiter::direct(throughput_quota)),
            post_frequency_limiter: Arc::new(RateLimiter::direct(post_quota)),
        }
    }

    /// Waits for both throughput and frequency limits to be available.
    pub async fn wait_for_capacity(&self, data_len: usize) -> crate::error::Result<()> {
        let start = Instant::now();

        // Wait for frequency capacity first
        self.post_frequency_limiter
            .until_n_ready(NonZeroU32::new(1).unwrap())
            .await?;

        // Wait for throughput capacity (only if data_len > 0)
        if data_len > 0 {
            if let Ok(data_len_nz) = NonZeroU32::try_from(data_len as u32) {
                self.throughput_limiter.until_n_ready(data_len_nz).await?;
            }
        }

        let elapsed = start.elapsed();
        if elapsed > Duration::from_millis(100) {
            let elapsed_formatted = humantime::format_duration(elapsed);
            info!("Was throttled for {elapsed_formatted}");
        }

        Ok(())
    }

    /// Checks if the request can proceed without waiting.
    /// Returns true if both throughput and frequency limits allow the request.
    pub fn can_proceed(&self, data_len: usize) -> bool {
        let data_len_u32 = u32::try_from(data_len).unwrap_or(u32::MAX);
        let throughput_ok = if data_len_u32 == 0 {
            true // Zero-length requests don't consume throughput
        } else {
            match self
                .throughput_limiter
                .check_n(NonZeroU32::new(data_len_u32).unwrap())
            {
                Ok(Ok(())) => true,
                Ok(Err(_)) | Err(_) => false,
            }
        };

        let frequency_ok = match self
            .post_frequency_limiter
            .check_n(NonZeroU32::new(1).expect("qed"))
        {
            Ok(Ok(())) => true,
            Ok(Err(_)) | Err(_) => false,
        };

        throughput_ok && frequency_ok
    }
}

#[allow(non_snake_case)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    fn create_test_throttler() -> Throttler {
        Throttler::new(Throughput {
            bytes_per_sec: NonZeroU32::new(100).unwrap(),
            max_burst: NonZeroU32::new(50).unwrap(),
            calls_per_sec: NonZeroU32::new(2).unwrap(),
        })
    }

    #[tokio::test]
    async fn can_proceed__within_limits__returns_true() {
        // given
        let throttler = create_test_throttler();

        // when
        let actual = throttler.can_proceed(10);

        // then
        assert!(actual);
    }

    #[tokio::test]
    async fn can_proceed__after_exhausting_throughput_limit__returns_false() {
        // given
        let throttler = create_test_throttler();
        let _ = throttler.can_proceed(50); // use burst capacity
        let _ = throttler.can_proceed(50); // exhaust remaining capacity

        // when
        let actual = throttler.can_proceed(1);

        // then
        assert!(!actual);
    }

    #[tokio::test]
    async fn can_proceed__after_exhausting_frequency_limit__returns_false() {
        // given
        let throttler = create_test_throttler();
        throttler.can_proceed(1); // first call
        throttler.can_proceed(1); // second call

        // when
        let actual = throttler.can_proceed(1);

        // then
        assert!(!actual);
    }

    #[tokio::test]
    async fn wait_for_capacity__when_within_limits__completes_quickly() {
        // given
        let throttler = create_test_throttler();
        let start = std::time::Instant::now();

        // when
        throttler.wait_for_capacity(10).await.unwrap();

        // then
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn wait_for_capacity__when_limits_exhausted__waits_before_proceeding() {
        // given
        let throttler = create_test_throttler();
        assert!(throttler.can_proceed(50)); // use throughput but not frequency
        throttler.wait_for_capacity(1).await.unwrap(); // use first frequency slot
        throttler.wait_for_capacity(1).await.unwrap(); // use second frequency slot
        let start = std::time::Instant::now();

        // when
        throttler.wait_for_capacity(1).await.unwrap();

        // then
        let elapsed = start.elapsed();
        assert!(elapsed > Duration::from_millis(400)); // should wait for frequency reset
    }

    #[tokio::test]
    async fn can_proceed__with_burst_sized_request__returns_true() {
        // given
        let throttler = create_test_throttler();

        // when
        let actual = throttler.can_proceed(50); // max_burst size

        // then
        assert!(actual);
    }

    #[tokio::test]
    async fn can_proceed__after_using_full_burst__returns_false() {
        // given
        let throttler = create_test_throttler();
        throttler.can_proceed(50); // use full burst

        // when
        let actual = throttler.can_proceed(1);

        // then
        assert!(!actual);
    }

    #[tokio::test]
    async fn can_proceed__after_frequency_limit_resets__returns_true() {
        // given
        let throttler = create_test_throttler();
        throttler.wait_for_capacity(1).await.unwrap(); // first call
        throttler.wait_for_capacity(1).await.unwrap(); // second call
        sleep(Duration::from_millis(600)).await; // wait for reset

        // when
        let actual = throttler.can_proceed(1);

        // then
        assert!(actual);
    }

    #[tokio::test]
    async fn wait_for_capacity__with_concurrent_requests__all_complete() {
        // given
        let throttler = Arc::new(create_test_throttler());
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let throttler = throttler.clone();
                tokio::spawn(async move {
                    throttler.wait_for_capacity(10).await.unwrap();
                })
            })
            .collect();

        // when
        let results = timeout(Duration::from_secs(5), async {
            for handle in handles {
                handle.await.unwrap();
            }
        })
        .await;

        // then
        assert!(results.is_ok());
    }

    #[tokio::test]
    async fn can_proceed__with_zero_size_request__consumes_frequency_limit() {
        // given
        let throttler = create_test_throttler();
        throttler.can_proceed(0); // first call
        throttler.can_proceed(0); // second call

        // when
        let actual = throttler.can_proceed(0);

        // then
        assert!(!actual);
    }

    #[tokio::test]
    async fn can_proceed__with_oversized_request__returns_false() {
        // given
        let throttler = create_test_throttler();

        // when
        let actual = throttler.can_proceed(200); // larger than burst

        // then
        assert!(!actual);
    }

    #[tokio::test]
    async fn wait_for_capacity__when_can_proceed_returns_true__completes_immediately() {
        // given
        let throttler = create_test_throttler();
        let data_len = 10;
        assert!(throttler.can_proceed(data_len)); // verify can_proceed is true
        let start = std::time::Instant::now();

        // when
        let result = throttler.wait_for_capacity(data_len).await;

        // then
        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(10)); // should be nearly instantaneous
    }
}
