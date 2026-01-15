use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::mpsc::{SendError, TryRecvError};

pub(crate) trait UnboundedSender<T: SendOnMt + Unpin + 'static>: Clone {
    /// Send a message, or return `Err` if channel is closed.
    fn send(&self, value: T) -> Result<(), SendError>;

    /// Returns `true` if channel is closed.
    fn is_closed(&self) -> bool;
}

pub(crate) trait UnboundedReceiver<T: SendOnMt + Unpin + 'static> {
    /// Wait and receive a message, or `None` if channel is closed.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe.
    /// 
    /// If `recv` is used in `futures::select!` statement and some other branch completes first,
    /// it is guaranteed that no messages were received on this channel.
    fn recv(&mut self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Try to receive a message,
    /// or `Err` if channel is closed or no message available at the moment.
    fn try_recv(&mut self) -> Result<T, TryRecvError>;

    /// Returns `true` if channel is closed.
    fn is_closed(&self) -> bool;

    /// Returns the internal queue length.
    fn len(&self) -> usize;

    /// Returns `true` if the internal queue is empty.
    fn is_empty(&self) -> bool;
}


conditional! {
    multithread,

    pub(crate) mod mt;

    /// Unbounded Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type Sender<T> = mt::Sender<T>;

    /// Unbounded Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type Receiver<T> = mt::Receiver<T>;

    /// Get an unbounded channel depending on the current async environment.
    ///
    /// Returns **thread safe** channel.
    pub(crate) fn channel<T: Send + Unpin + 'static>() -> (Sender<T>, Receiver<T>) {
        mt::channel()
    }
}

conditional! {
    not(multithread),

    pub(crate) mod st;

    /// Unbounded Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type Sender<T> = st::Sender<T>;

    /// Unbounded Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type Receiver<T> = st::Receiver<T>;

    /// Get an unbounded channel depending on the current async environment.
    ///
    /// Returns **not thread safe** channel.
    pub(crate) fn channel<T: Unpin + 'static>() -> (Sender<T>, Receiver<T>) {
        st::channel()
    }
}
