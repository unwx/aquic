use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::TryRecvError;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum SendError<T> {
    /// There is already a message that has been sent.
    ///
    /// Contains a clone of the "already sent message".
    Full(T),

    /// The channel is empty and closed.
    Closed,
}

impl<T> Debug for SendError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full(_) => f.debug_tuple("Full").finish(),
            Self::Closed => write!(f, "Closed"),
        }
    }
}

impl<T> Display for SendError<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full(_) => write!(f, "channel is full"),
            Self::Closed => write!(f, "channel is closed"),
        }
    }
}

impl<T> Error for SendError<T> {}


/// A sending `mpmc` oneshot part.
pub trait OneshotSender<T>: Clone {
    /// Sends a message.
    ///
    /// It is guaranteed that no more messages can be sent on this channel.
    fn send(&self, value: T) -> Result<(), SendError<T>>;

    /// Returns `true` if no [`OneshotReceiver`] exists.
    fn is_closed(&self) -> bool;
}

/// A receiving `mpmc` oneshot part.
pub trait OneshotReceiver<T>: Clone {
    /// Waits for a message and get its clone.
    ///
    /// Further calls will return the same message immediately.
    ///
    /// Returns `None` if the channel is closed and no message is available.
    ///
    /// # Cancel safety
    ///
    /// Safe, no side effects.
    fn recv(&self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Tries to receive a message immediately.
    fn try_recv(&self) -> Result<T, TryRecvError>;

    /// Returns `true` if no [`OneshotSender`] exists.
    fn is_closed(&self) -> bool;
}


conditional! {
    multithread,

    mod mt;

    /// Async runtime agnostic `oneshot::Sender`.
    pub type Sender<T> = mt::Sender<T>;

    /// Async runtime agnostic `oneshot::Receiver`.
    pub type Receiver<T> = mt::Receiver<T>;

    /// Creates a `mpmc` oneshot channel.
    ///
    /// This channel may hold only a single value,
    /// that can be retrieved multiple times (by cloning it) by multiple receivers.
    #[inline]
    pub fn channel<T: Clone + Send + Sync>() -> (Sender<T>, Receiver<T>) {
        mt::channel()
    }
}

conditional! {
    not(multithread),

    mod st;

    /// Async runtime agnostic `oneshot::Sender`.
    pub type Sender<T> = st::Sender<T>;

    /// Async runtime agnostic `oneshot::Receiver`.
    pub type Receiver<T> = st::Receiver<T>;

    /// Creates a `mpmc` oneshot channel.
    ///
    /// This channel may hold only a single value,
    /// that can be retrieved multiple times (by cloning it) by multiple receivers.
    #[inline]
    pub fn channel<T: Clone>() -> (Sender<T>, Receiver<T>) {
        st::channel()
    }
}
