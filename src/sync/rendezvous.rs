use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

//noinspection DuplicatedCode
/// Creates a new synchronous, `SPSC` rendezvous channel.
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
    /// Returns `Ok` if the message was delivered successfully, or `Err`
    /// with the original message inside if the channel is closed.
    ///
    /// # Cancel Safety
    ///
    /// Canceling this future breaks the strict rendezvous guarantee,
    /// but will not lose the message.
    ///
    /// If this method is canceled while waiting for the receiver, the message remains
    /// in the queue. Consequently, subsequent calls to [`Sender::send()`] may complete
    /// immediately (behaving like a buffered channel) until the [`Receiver`] drains
    /// the backlog.
    pub async fn send(&mut self, value: T) -> Result<(), T> {
        {
            let mut state = self.shared.state.lock().unwrap();
            if !state.open {
                return Err(value);
            }

            state.messages.push_back(value);
        };

        self.shared.send_callback.call();
        self.shared.recv_callback.wait().await;

        Ok(())
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
        self.shared.send_callback.wait().await;

        let message = {
            let mut state = self.shared.state.lock().unwrap();
            state.messages.pop_front()
        };

        if message.is_some() {
            self.shared.recv_callback.call();
        }

        message
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

    /// Callback used to notify that a message was sent.
    send_callback: Callback,

    /// Callback used to notify that a message was received.
    recv_callback: Callback,
}

impl<T> Shared<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                messages: VecDeque::with_capacity(1),
                open: true,
            }),
            send_callback: Callback::new(),
            recv_callback: Callback::new(),
        }
    }

    fn close(&self) {
        {
            let mut state = self
                .state
                .lock()
                // Poisoned state may indicate that some messages could be lost.
                // To avoid sophisticated vulnerabilities on missing messages,
                // we better panic here, than recover.
                .unwrap();

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
    pub messages: VecDeque<T>,

    /// Whether the channel is open.
    pub open: bool,
}

#[derive(Debug)]
struct Callback {
    /// We use Semaphore to make & listen for callbacks.
    ///
    /// - `semaphore.acquire().forget()` to wait for a callback.
    /// - `semaphore.add_permits(1)` to make one.
    /// - `semaphore.close()` to make a callback and close.
    semaphore: Semaphore,
}

impl Callback {
    pub fn new() -> Self {
        Self {
            semaphore: Semaphore::new(0),
        }
    }

    pub async fn wait(&self) {
        if let Ok(permit) = self.semaphore.acquire().await {
            permit.forget();
        }
    }

    pub fn call(&self) {
        self.semaphore.add_permits(1)
    }

    pub fn close(&self) {
        self.semaphore.close();
    }
}
