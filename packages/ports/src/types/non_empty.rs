pub use nonempty::{nonempty, NonEmpty};

pub trait CollectNonEmpty: Iterator {
    fn collect_nonempty(self) -> Option<NonEmpty<Self::Item>>
    where
        Self: Sized,
    {
        NonEmpty::collect(self)
    }
}
impl<I: Iterator> CollectNonEmpty for I {}

pub trait TryCollectNonEmpty: Iterator<Item = std::result::Result<Self::Ok, Self::Err>> {
    type Ok;
    type Err;

    fn try_collect_nonempty(self) -> Result<Option<NonEmpty<Self::Ok>>, Self::Err>
    where
        Self: Sized,
        Self::Err: std::error::Error,
    {
        let collected: Result<Vec<_>, _> = self.collect();
        collected.map(NonEmpty::collect)
    }
}

// Now implement the trait for any iterator that produces `Result` items
impl<I, T, E> TryCollectNonEmpty for I
where
    I: Iterator<Item = Result<T, E>>,
    E: std::error::Error,
{
    type Ok = T;
    type Err = E;
}
