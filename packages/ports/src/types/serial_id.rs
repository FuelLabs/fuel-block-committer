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
pub struct NonNegativeI32 {
    id: i32,
}

impl NonNegativeI32 {
    pub fn as_u32(&self) -> u32 {
        self.id as u32
    }
}

impl TryFrom<u32> for NonNegativeI32 {
    type Error = InvalidConversion;
    fn try_from(id: u32) -> Result<Self, Self::Error> {
        if id > i32::MAX as u32 {
            return Err(InvalidConversion {
                message: format!("{id} is too large for i32"),
            });
        }
        Ok(Self { id: id as i32 })
    }
}
