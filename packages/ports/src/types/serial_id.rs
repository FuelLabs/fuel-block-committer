use std::fmt::Display;

#[derive(Debug, Clone)]
pub struct InvalidConversion {
    pub message: String,
}

impl std::fmt::Display for InvalidConversion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid numeric id: {}", self.message)
    }
}

impl std::error::Error for InvalidConversion {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NonNegative<NUM> {
    val: NUM,
}

impl<NUM: Display> Display for NonNegative<NUM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.val.fmt(f)
    }
}

impl<NUM: Default> Default for NonNegative<NUM> {
    fn default() -> Self {
        Self {
            val: Default::default(),
        }
    }
}

impl NonNegative<i32> {
    pub fn as_u32(&self) -> u32 {
        self.val as u32
    }

    pub fn as_i32(&self) -> i32 {
        self.val
    }
}

impl NonNegative<i64> {
    pub fn as_u64(&self) -> u64 {
        self.val as u64
    }

    pub fn as_i64(&self) -> i64 {
        self.val
    }
}

impl From<u32> for NonNegative<i64> {
    fn from(value: u32) -> Self {
        Self {
            val: i64::from(value),
        }
    }
}

impl TryFrom<i32> for NonNegative<i32> {
    type Error = InvalidConversion;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(InvalidConversion {
                message: format!("{value} is negative"),
            });
        }
        Ok(Self { val: value })
    }
}

impl TryFrom<i64> for NonNegative<i64> {
    type Error = InvalidConversion;
    fn try_from(id: i64) -> Result<Self, Self::Error> {
        if id < 0 {
            return Err(InvalidConversion {
                message: format!("{id} is negative"),
            });
        }
        Ok(Self { val: id })
    }
}

impl TryFrom<u32> for NonNegative<i32> {
    type Error = InvalidConversion;
    fn try_from(id: u32) -> Result<Self, Self::Error> {
        if id > i32::MAX as u32 {
            return Err(InvalidConversion {
                message: format!("{id} is too large for i32"),
            });
        }
        Ok(Self { val: id as i32 })
    }
}

#[cfg(feature = "test-helpers")]
impl rand::distributions::Distribution<NonNegative<i32>> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> NonNegative<i32> {
        NonNegative::try_from(rng.gen_range(0..=i32::MAX)).expect("to generate a non-negative i32")
    }
}
