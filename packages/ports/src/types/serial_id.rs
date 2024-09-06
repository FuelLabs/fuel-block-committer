#[derive(Debug, Clone)]
pub struct InvalidNumericId {
    pub message: String,
}

impl std::fmt::Display for InvalidNumericId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid numeric id: {}", self.message)
    }
}

impl std::error::Error for InvalidNumericId {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumericId {
    id: i32,
}
