use std::fmt::Display;
use std::hash::Hash;

pub(crate) mod buffer;
pub(crate) mod future;
pub(crate) mod stream;

pub trait ApplicationError: Send + Copy + Eq + Hash + From<u64> + Into<u64> + Display + 'static {
    fn internal() -> Self;
}
