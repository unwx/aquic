use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::{SendError, TryRecvError};

/// A sending part of unbounded `mpmc` channel.
pub(crate) trait UnboundedSender<T>: Clone {
    /// Sends a message if channel is open.
    fn send(&self, value: T) -> Result<(), SendError<T>>;

    /// Returns `true` if [`UnboundedReceiver`] is dropped.
    fn is_closed(&self) -> bool;
}

/// A receiving part of unbounded `mpmc` channel.
pub(crate) trait UnboundedReceiver<T> {
    /// Waits and receive a message, or returns `None` if channel is closed.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that no messages were received on this channel
    /// if the future drops.
    fn recv(&mut self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Tries to receive a message immediately.
    fn try_recv(&mut self) -> Result<T, TryRecvError>;

    /// Returns `true` if no [`UnboundedSender`] exists.
    fn is_closed(&self) -> bool;

    /// Returns the internal queue length.
    fn len(&self) -> usize;

    /// Returns `true` if the internal queue is empty.
    fn is_empty(&self) -> bool;
}


conditional! {
    multithread,

    mod mt;

    /// Async runtime agnostic `unbounded::Sender`.
    pub(crate) type Sender<T> = mt::Sender<T>;

    /// Async runtime agnostic `unbounded::Receiver`.
    pub(crate) type Receiver<T> = mt::Receiver<T>;

    /// Creates an unbounded `mpmc` channel.
    #[inline]
    pub(crate) fn channel<T: Send + Unpin + 'static>() -> (Sender<T>, Receiver<T>) {
        mt::channel()
    }
}

conditional! {
    not(multithread),

    mod st;

    /// Async runtime agnostic `unbounded::Sender`.
    pub(crate) type Sender<T> = st::Sender<T>;

    /// Async runtime agnostic `unbounded::Receiver`.
    pub(crate) type Receiver<T> = st::Receiver<T>;

    /// Creates an unbounded `mpmc` channel.
    #[inline]
    pub(crate) fn channel<T: Unpin + 'static>() -> (Sender<T>, Receiver<T>) {
        st::channel()
    }
}
