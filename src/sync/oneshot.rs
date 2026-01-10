use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, RwLock};
use tokio::sync::Semaphore;

/// Message delivery failure.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SendError {
    /// There is already a message that has been **sent**.
    Full,

    /// The channel is closed.
    Closed,
}

impl Display for SendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Full => write!(f, "channel is full"),
            SendError::Closed => write!(f, "channel is closed"),
        }
    }
}

impl Error for SendError {}


/// `try_recv()` error.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum TryRecvError {
    /// This channel is currently empty, but the **Sender**(s) have not yet
    /// disconnected, so data may yet become available.
    Empty,

    /// The channel's sending half has become disconnected, and there will
    /// never be any more data received on it.
    Disconnected,
}

impl Display for TryRecvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TryRecvError::Empty => write!(f, "channel is empty"),
            TryRecvError::Disconnected => write!(f, "channel is closed"),
        }
    }
}

impl Error for TryRecvError {}


//noinspection DuplicatedCode
/// Creates a new `MPMC` one-shot channel.
///
/// The channel stores a single value;
/// once sent, all current and future waiters on the receiver are notified immediately.
///
/// Unlike standard oneshots, this channel preserves the signal, allowing late
/// receivers to receive instantly.
#[rustfmt::skip]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared::new());
    let sender = Sender { shared: shared.clone() };
    let receiver = Receiver { shared };

    (sender, receiver)
}


/// Oneshot sender: sends message only once,
/// and prevents all other attempts to rewrite it.
#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Send a message if there were no messages before, and channel is open.
    ///
    /// Upon successful end of this method, it is guaranteed that all `Receiver`s
    /// are able to get this message.
    pub fn send(&self, value: T) -> Result<(), SendError> {
        {
            let mut state = self.shared.state.write().unwrap();

            if state.value.is_some() {
                return Err(SendError::Full);
            }
            if !state.open {
                return Err(SendError::Closed);
            }

            state.value = Some(value);
        }

        self.shared.callback.call();
        Ok(())
    }

    /// Close the channel and notify the [`Receiver`].
    ///
    /// [`Receiver`] will still be able to get the message.
    pub fn close(&self) {
        self.shared.close();
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}


/// Oneshot receiver: receives and stores the message.
///
/// Once the message is received, it will always be available and will never change.
#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Receiver<T> {
    /// Close the channel.
    ///
    /// Keeps the message untouched and ready to collect anytime.
    pub fn close(&self) {
        self.shared.close();
    }
}

impl<T: Clone> Receiver<T> {
    /// Wait for a message and get its clone.
    ///
    /// Returns `None` if the channel is closed and no message is available.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe.
    pub async fn recv(&self) -> Option<T> {
        self.shared.callback.wait().await;
        self.shared.state.read().unwrap().value.clone()
    }

    /// Attempts to receive a value from the channel without blocking.
    ///
    /// If a message is available, it is cloned and returned.
    ///
    /// If no message is available or the channel is not in a ready state,
    /// it returns an error immediately.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let state = self.shared.state.read().unwrap();
        state.value.clone().ok_or_else(|| {
            if state.open {
                TryRecvError::Empty
            } else {
                TryRecvError::Disconnected
            }
        })
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}


/// Shared between [`Sender`] and [`Receiver`] data.
#[derive(Debug)]
struct Shared<T> {
    pub state: RwLock<State<T>>,
    pub callback: Callback,
}

impl<T> Shared<T> {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(State {
                value: None,
                open: true,
            }),
            callback: Callback::new(),
        }
    }

    pub fn close(&self) {
        {
            let mut state = self.state.write().unwrap();
            state.open = false;
        }

        self.callback.close();
    }
}

/// Internal channel's state.
#[derive(Debug)]
struct State<T> {
    /// Stored message.
    pub value: Option<T>,

    /// Whether channel is open or not.
    pub open: bool,
}

#[derive(Debug)]
struct Callback {
    semaphore: Semaphore,
}

impl Callback {
    pub fn new() -> Self {
        Self {
            semaphore: Semaphore::new(0),
        }
    }

    pub async fn wait(&self) {
        let _ = self
            .semaphore
            .acquire()
            .await
            .expect("use of callback after it was closed");
    }

    pub fn call(&self) {
        self.semaphore.add_permits(Semaphore::MAX_PERMITS);
    }

    pub fn close(&self) {
        self.semaphore.close();
    }
}
