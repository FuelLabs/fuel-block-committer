#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct EthHeight {
    height: i64,
}

#[derive(Debug, Clone)]
pub struct InvalidEthHeight(String);
impl std::fmt::Display for InvalidEthHeight {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Invalid eth height: {}", self.0)
    }
}
impl std::error::Error for InvalidEthHeight {}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<EthHeight> for rand::distributions::Standard {
    fn sample<R: rand::prelude::Rng + ?Sized>(&self, rng: &mut R) -> EthHeight {
        let height: i64 = rng.gen_range(0..=i64::MAX);
        height.try_into().expect("Must be valid EthHeight")
    }
}

impl TryFrom<i64> for EthHeight {
    type Error = InvalidEthHeight;

    fn try_from(height: i64) -> Result<Self, Self::Error> {
        if height < 0 {
            return Err(InvalidEthHeight(format!(
                "must be non-negative, got {height}",
            )));
        }
        Ok(Self { height })
    }
}

impl TryFrom<u64> for EthHeight {
    type Error = InvalidEthHeight;
    fn try_from(height: u64) -> Result<Self, Self::Error> {
        if height >= i64::MAX as u64 {
            return Err(InvalidEthHeight(format!(
                "{height} too large. DB can handle at most {}",
                i64::MAX
            )));
        }
        Ok(Self {
            height: height as i64,
        })
    }
}

impl From<u32> for EthHeight {
    fn from(height: u32) -> Self {
        Self {
            height: i64::from(height),
        }
    }
}

impl From<EthHeight> for i64 {
    fn from(height: EthHeight) -> Self {
        height.height
    }
}

impl From<EthHeight> for u64 {
    fn from(height: EthHeight) -> Self {
        height.height as Self
    }
}
