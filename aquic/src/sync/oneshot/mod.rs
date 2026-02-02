use crate::conditional;
use crate::exec::{SendOnMt, SyncOnMt};
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SendError {
    /// There is already a message that has been sent.
    Full,

    /// The channel is closed.
    Closed,
}

impl Display for SendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "channel is full"),
            Self::Closed => write!(f, "channel is closed"),
        }
    }
}

impl Error for SendError {}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum TryRecvError {
    /// This channel is currently empty, but the senders have not yet
    /// dropped, so data may yet become available.
    Empty,

    /// All the senders have been dropped, and there is no message available.
    Closed,
}

//noinspection DuplicatedCode
impl Display for TryRecvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "channel is empty"),
            Self::Closed => write!(f, "channel is closed"),
        }
    }
}

impl Error for TryRecvError {}


/*
 * Oneshot channel requires `T` to be sync on multithread async envs,
 * to avoid using Mutex for cloning the `T` on each `recv()`.
 *
 * In most cases oneshot channels are used for some sort of cheap signals,
 * that are going to be Sync without manual synchronization.
 *
 * Therefore, I believe, this design decision should not be a problem?
 * In case of any trouble, we can remove Sync restriction without breaking backward compatibility,
 * though the clients might want to remove extra Mutex after.
 */

/// Sends message only once,
/// and guarantees that no more messages will be sent.
pub trait OneshotSender<T: SendOnMt + SyncOnMt>: Clone {
    /// Send a message if there were no messages before,
    /// and at least one receiver exists.
    fn send(&self, value: T) -> Result<(), SendError>;

    /// Returns `true` if there is no `Receiver` alive.
    fn is_closed(&self) -> bool;
}

/// Receives a message,
/// and guarantees that no more messages will be received.
pub trait OneshotReceiver<T: Clone + SendOnMt + SyncOnMt>: Clone {
    /// Wait for a message and get its clone.
    ///
    /// Returns `None` if the channel is closed and no message is available.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe,
    /// the original message never leaves the `Receiver`.
    fn recv(&self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Return a clone of the message if it exists,
    /// or error if it doesn't, or the channel is closed.
    fn try_recv(&self) -> Result<T, TryRecvError>;

    /// Returns `true` if there is no `Sender` alive.
    fn is_closed(&self) -> bool;
}


conditional! {
    multithread,

    pub mod mt;

    /// Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub type Sender<T> = mt::Sender<T>;

    /// Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub type Receiver<T> = mt::Receiver<T>;

    /// Returns `mpmc` channel that can hold only a single value,
    /// that can always be retrieved multiple times (by cloning it) by a single/multiple receivers.
    ///
    /// On current async environment it's **thread safe**,
    /// and intended to be used in multi-thread environments.
    pub fn channel<T: Send + Sync>() -> (Sender<T>, Receiver<T>) {
        mt::channel()
    }
}

conditional! {
    not(multithread),

    pub mod st;

    /// Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub type Sender<T> = st::Sender<T>;

    /// Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub type Receiver<T> = st::Receiver<T>;

    /// Returns `mpmc` channel that can hold only a single value,
    /// that can always be retrieved multiple times (by cloning it) by a single/multiple receivers.
    ///
    /// On current async environment it's **not thread safe**,
    /// and intended to be used in multi-thread environments.
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        st::channel()
    }
}
