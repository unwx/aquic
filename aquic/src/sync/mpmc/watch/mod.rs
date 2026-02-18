use crate::{
    conditional,
    exec::SendOnMt,
    sync::{SendError, TryRecvError},
};

/// A sending `mpmc` watch part.
///
/// It is generally used for updates like: configs, constraints, etc.
pub trait WatchSender<T>: Clone {
    /// Replaces an old value with the new one.
    ///
    /// The value will be visible to each existing [`WatchReceiver`].
    fn send(&self, value: T) -> Result<(), SendError<T>>;

    /// Returns `true` if no [`WatchReceiver`] exists.
    fn is_closed(&self) -> bool;
}

/// A receiving `mpmc` watch part.
///
/// Each receiver tracks seen and unseen updates individually.
pub trait WatchReceiver<T>: Clone {
    /// Waits for a new update and returns it.
    ///
    /// The update may be viewed only once.
    ///
    /// Returns `None` if channel is closed.
    fn recv(&mut self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Tries to receive an update immediately if it exists.
    fn try_recv(&mut self) -> Result<T, TryRecvError>;

    /// Returns a last update if it exists, even if it was seen earlier.
    fn try_recv_any(&mut self) -> Result<T, TryRecvError>;

    /// Returns `true` if no [`WatchSender`] exists.
    fn is_closed(&self) -> bool;
}


conditional! {
    multithread,

    mod mt;

    /// Async runtime agnostic `watch::Sender`.
    pub type Sender<T> = mt::Sender<T>;

    /// Async runtime agnostic `watch::Receiver`.
    pub type Receiver<T> = mt::Receiver<T>;

    /// Creates a `watch` channel.
    ///
    /// It is generally used for updates like: new config values, constraints, etc.
    #[inline]
    pub fn channel<T: Send + Sync + Clone>() -> (Sender<T>, Receiver<T>) {
        mt::channel()
    }
}

conditional! {
    not(multithread),

    mod st;

    /// Async runtime agnostic `watch::Sender`.
    pub type Sender<T> = st::Sender<T>;

    /// Async runtime agnostic `watch::Receiver`.
    pub type Receiver<T> = st::Receiver<T>;

    /// Creates a `watch` channel.
    ///
    /// It is generally used for updates like: new config values, constraints, etc.
    #[inline]
    pub fn channel<T: Clone>() -> (Sender<T>, Receiver<T>) {
        st::channel()
    }
}
