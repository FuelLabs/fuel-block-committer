#[cfg(feature = "l1")]
pub use alloy::primitives::{Address, U256};
#[cfg(any(feature = "l1", feature = "storage"))]
pub use futures::Stream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyVec<T> {
    vec: Vec<T>,
}

#[macro_export]
macro_rules! non_empty_vec {
    ($($x:expr),+) => {
        NonEmptyVec {
            vec: vec![$($x),+]
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecIsEmpty;

impl std::fmt::Display for VecIsEmpty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vec cannot be empty")
    }
}

impl<T> TryFrom<Vec<T>> for NonEmptyVec<T> {
    type Error = VecIsEmpty;

    fn try_from(value: Vec<T>) -> std::result::Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(VecIsEmpty);
        }
        Ok(Self { vec: value })
    }
}

impl<T> NonEmptyVec<T> {
    pub fn into_inner(self) -> Vec<T> {
        self.vec
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    pub fn inner(&self) -> &Vec<T> {
        &self.vec
    }
}

mod block_submission;
#[cfg(feature = "l1")]
mod fuel_block_committed_on_l1;
mod l1_height;
mod serial_id;
mod state_submission;

pub use block_submission::*;
#[cfg(feature = "l1")]
pub use fuel_block_committed_on_l1::*;
pub use l1_height::*;
pub use serial_id::*;
pub use state_submission::*;
#[cfg(any(feature = "fuel", feature = "l1"))]
pub use validator::block::*;
