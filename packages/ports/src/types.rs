use std::{num::NonZeroUsize, ops::Index};

#[cfg(feature = "l1")]
pub use alloy::primitives::{Address, U256};
#[cfg(any(feature = "l1", feature = "storage"))]
pub use futures::Stream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyVec<T> {
    vec: Vec<T>,
}

impl<T> Index<usize> for NonEmptyVec<T> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        &self.vec[index]
    }
}

#[macro_export]
macro_rules! non_empty_vec {
    ($($x:expr),+) => {
        $crate::types::NonEmptyVec::try_from(vec![$($x),+]).unwrap()
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
    pub fn first(&self) -> &T {
        self.vec.first().expect("vec is not empty")
    }

    pub fn last(&self) -> &T {
        self.vec.last().expect("vec is not empty")
    }

    pub fn take_first(self) -> T {
        self.vec.into_iter().next().expect("vec is not empty")
    }

    pub fn into_inner(self) -> Vec<T> {
        self.vec
    }

    pub fn len(&self) -> NonZeroUsize {
        self.vec.len().try_into().expect("vec is not empty")
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn inner(&self) -> &Vec<T> {
        &self.vec
    }
}

mod block_submission;
mod fragment;
#[cfg(feature = "l1")]
mod fuel_block_committed_on_l1;
mod l1_height;
mod serial_id;
mod state_submission;

pub use block_submission::*;
pub use fragment::*;
#[cfg(feature = "l1")]
pub use fuel_block_committed_on_l1::*;
pub use l1_height::*;
pub use serial_id::*;
pub use state_submission::*;
