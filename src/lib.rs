#![forbid(unsafe_code)]

use std::fmt::Display;
use std::hash::Hash;

mod buffer;
mod sync;
mod stream;

pub trait ApplicationError:
    Send + Sync + Copy + Eq + Hash + From<u64> + Into<u64> + Display + 'static
{
    fn internal() -> Self;
}
