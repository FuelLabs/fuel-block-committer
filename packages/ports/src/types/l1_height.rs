#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct L1Height {
    height: i64,
}

#[derive(Debug, Clone)]
pub struct InvalidL1Height(String);
impl std::fmt::Display for InvalidL1Height {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Invalid l1 height: {}", self.0)
    }
}
impl std::error::Error for InvalidL1Height {}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<L1Height> for rand::distributions::Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> L1Height {
        let height: i64 = rng.gen_range(0..=i64::MAX);
        height.try_into().expect("Must be valid EthHeight")
    }
}

impl TryFrom<i64> for L1Height {
    type Error = InvalidL1Height;

    fn try_from(height: i64) -> Result<Self, Self::Error> {
        if height < 0 {
            return Err(InvalidL1Height(format!(
                "must be non-negative, got {height}",
            )));
        }
        Ok(Self { height })
    }
}

impl TryFrom<u64> for L1Height {
    type Error = InvalidL1Height;
    fn try_from(height: u64) -> Result<Self, Self::Error> {
        if height >= i64::MAX as u64 {
            return Err(InvalidL1Height(format!(
                "{height} too large. DB can handle at most {}",
                i64::MAX
            )));
        }
        Ok(Self {
            height: height as i64,
        })
    }
}

impl From<u32> for L1Height {
    fn from(height: u32) -> Self {
        Self {
            height: i64::from(height),
        }
    }
}

impl From<L1Height> for i64 {
    fn from(height: L1Height) -> Self {
        height.height
    }
}

impl From<L1Height> for u64 {
    fn from(height: L1Height) -> Self {
        height.height as Self
    }
}
