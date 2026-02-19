use crate::conditional;
use std::fmt::{self, Display, Formatter};

pub mod dgram;
pub mod mpmc;
pub mod mpsc;
pub mod stream;

pub(crate) mod util;


/// Channel is closed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SendError<T>(pub T);

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish()
    }
}

impl<T> Display for SendError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "channel is closed")
    }
}

impl<T> std::error::Error for SendError<T> {}


#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrySendError {
    /// Channel has no more capacity.
    Full,

    /// Channel is closed.
    Closed,
}

impl Display for TrySendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full => write!(f, "channel is full"),
            TrySendError::Closed => write!(f, "channel is closed"),
        }
    }
}

impl std::error::Error for TrySendError {}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum TryRecvError {
    /// Channel is empty.
    Empty,

    /// No `Sender` exists, no message available.
    Closed,
}

impl Display for TryRecvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "channel is empty"),
            Self::Closed => write!(f, "channel is closed"),
        }
    }
}

impl std::error::Error for TryRecvError {}


conditional! {
    multithread,

    use std::sync::Arc;

    /// Async runtime agnostic reference-counting pointer.
    pub(crate) type SmartRc<T> = Arc<T>;
}

conditional! {
    not(multithread),

    use std::rc::Rc;

    /// Async runtime agnostic reference-counting pointer.
    pub(crate) type SmartRc<T> = Rc<T>;
}
