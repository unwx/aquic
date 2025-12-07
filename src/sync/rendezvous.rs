use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

//noinspection DuplicatedCode
/// Creates a new synchronous (rendezvous) channel.
///
/// # What is a Rendezvous Channel?
///
/// Unlike a buffered channel (where you drop a message and leave),
/// a rendezvous channel requires both parties to meet to exchange the message.
///
/// The [`Sender::send`] method will wait
/// until the [`Receiver`] has actively arrived and taken the message.
///
/// # Cancel Safety
///
/// While the channel is designed to be synchronous, cancelling a `send` future
/// (e.g., via `tokio::select!`) may leave the message in the internal queue.
/// If this happens, the channel effectively behaves like a buffered channel
/// until the backlog is consumed.
#[rustfmt::skip]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared::new());
    let sender = Sender { shared: shared.clone(), };
    let receiver = Receiver { shared };

    (sender, receiver)
}


/// The sending half of a rendezvous channel.
///
/// The channel closes automatically when this sender is dropped, or manually
/// via [`Sender::close()`].
///
/// See [`channel()`] for more details.
#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Sends a message and waits until it is received.
    ///
    /// Returns `true` if the message was delivered successfully, or `false`
    /// if the channel is closed.
    ///
    /// # Cancel Safety
    ///
    /// Canceling this future breaks the strict rendezvous guarantee.
    ///
    /// If this method is canceled while waiting for the receiver, the message remains
    /// in the queue. Consequently, subsequent calls to [`Sender::send()`] may complete
    /// immediately (behaving like a buffered channel) until the [`Receiver`] drains
    /// the backlog.
    pub async fn send(&mut self, value: T) -> bool {
        {
            let mut state = self.shared.state.lock().unwrap();
            if !state.open {
                return false;
            }

            state.messages.push_back(value);
        };

        self.shared.send_callback.add_permits(1);
        if let Ok(permit) = self.shared.recv_callback.acquire().await {
            permit.forget();
        }

        true
    }

    /// Closes the channel.
    ///
    /// Pending messages remain in the queue and can still be consumed by the [`Receiver`].
    pub fn close(&mut self) {
        self.shared.close();
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.close();
    }
}


/// The receiving half of a rendezvous channel.
///
/// The channel closes automatically when this receiver is dropped, or manually
/// via [`Receiver::close()`].
///
/// See [`channel()`] for more details.
#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Receiver<T> {
    /// Waits for and receives a message.
    ///
    /// Returns `Some(T)` if a message is available, or `None` if the channel
    /// is closed and empty.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel-safe. If the future is dropped before completion,
    /// no message is lost and the channel state remains consistent.
    pub async fn recv(&mut self) -> Option<T> {
        if let Ok(permit) = self.shared.send_callback.acquire().await {
            permit.forget();
        }

        self.shared
            .state
            .lock()
            .unwrap()
            .messages
            .pop_front()
            .inspect(|_| {
                self.shared.recv_callback.add_permits(1);
            })
    }

    /// Closes the channel.
    ///
    /// Pending messages remain in the queue and can still be consumed via [`Receiver::recv()`].
    pub fn close(&mut self) {
        self.shared.close();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}


/// Internal context shared between the [`Sender`] and [`Receiver`].
#[derive(Debug)]
struct Shared<T> {
    state: Mutex<State<T>>,

    /// Semaphore used to notify that a message was sent.
    send_callback: Semaphore,

    /// Semaphore used to notify that a message was received.
    recv_callback: Semaphore,
}

impl<T> Shared<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                messages: VecDeque::with_capacity(1),
                open: true,
            }),
            send_callback: Semaphore::new(0),
            recv_callback: Semaphore::new(0),
        }
    }

    fn close(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());

            state.open = false;
        }

        self.send_callback.close();
        self.recv_callback.close();
    }
}

#[derive(Debug)]
struct State<T> {
    /// Message buffer.
    ///
    /// In strict rendezvous operation, this contains at most one element.
    /// It may contain more if `send` futures are canceled.
    messages: VecDeque<T>,

    /// Whether the channel is open.
    open: bool,
}
