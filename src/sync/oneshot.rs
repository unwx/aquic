use std::sync::{Arc, RwLock};
use tokio_util::sync::CancellationToken;

//noinspection DuplicatedCode
/// Creates a new latching `MPMC` one-shot channel.
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


/// Message delivery failure.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SendError {
    /// There is already a message that **has** been sent.
    Full,

    /// The channel is closed.
    Closed,
}

/// Oneshot sender: sends message only once,
/// and prevents all other attempts to rewrite it.
#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Send a message if there were no messages before, and channel is open.
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

        self.shared.callback.cancel();
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
        self.shared.callback.cancelled().await;
        self.shared.state.read().unwrap().value.clone()
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
    state: RwLock<State<T>>,
    callback: CancellationToken,
}

impl<T> Shared<T> {
    fn new() -> Self {
        Self {
            state: RwLock::new(State {
                value: None,
                open: true,
            }),
            callback: CancellationToken::new(),
        }
    }

    fn close(&self) {
        {
            let mut state = self.state.write().unwrap();
            state.open = false;
        }

        self.callback.cancel();
    }
}

/// Internal channel's state.
#[derive(Debug)]
struct State<T> {
    /// Stored message.
    value: Option<T>,

    /// Whether channel is open or not.
    open: bool,
}
