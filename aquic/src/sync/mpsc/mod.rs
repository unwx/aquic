use std::error::Error;
use std::fmt::{Display, Formatter};

pub(crate) mod unbounded;
pub(crate) mod weighted;

/// Channel is closed.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SendError;

impl Display for SendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "channel is closed")
    }
}

impl Error for SendError {}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TryRecvError {
    /// This channel is currently empty, but the senders have not yet
    /// dropped, so data may yet become available.
    Empty,

    /// All the senders have been dropped, and there is no message available.
    Closed,
}

// noinspection DuplicatedCode
impl Display for TryRecvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "channel is empty"),
            Self::Closed => write!(f, "channel is closed"),
        }
    }
}

impl Error for TryRecvError {}
