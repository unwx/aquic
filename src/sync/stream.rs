use crate::stream::codec::{Decoder, Encoder};
use crate::stream::{Error, Payload, Priority};
use crate::sync::mpsc::weighted;
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender};
use crate::sync::oneshot;
use crate::sync::oneshot::{OneshotReceiver, OneshotSender, SendError, TryRecvError};
use crate::sync::stream::cell::PriorityCell;
use crate::{Estimate, Spec};
use futures::{FutureExt, select_biased};
use std::borrow::Cow;
use std::future::poll_fn;
use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::thread::panicking;

/// Creates a `spsc` channel for a single stream direction communication.
///
/// Also see [weighted] for more info about the underlying channel.
pub(crate) fn channel<S: Spec>(bound: usize) -> (Sender<S>, Receiver<S>) {
    let (item_sender, item_receiver) = weighted::channel(bound);
    let (error_sender, error_receiver) = oneshot::channel();
    let priority_cell = PriorityCell::new();

    let stream_sender = Sender {
        item_sender,
        error_sender: error_sender.clone(),
        error_receiver: error_receiver.clone(),
        priority_cell: priority_cell.clone(),
    };
    let stream_receiver = Receiver {
        item_receiver,
        error_sender,
        error_receiver,
        priority_cell,
    };

    (stream_sender, stream_receiver)
}


/// The sending half of a QUIC stream.
///
/// # Lifecycle
///
/// 1. **Active**: Data is sent using [`send()`](Sender::send).
/// 2. **Finishing**: The stream direction is gracefully closed using [`finish()`](Sender::finish)
///    or [`send_final()`](Sender::send_final), which sends a `FIN` bit to the peer.
/// 3. **Aborting**: The stream direction can be abruptly terminated using [`reset()`](Sender::reset),
///    sending a `RESET_STREAM` frame.
///
/// # Drop Behavior
///
/// If the `Sender` is dropped without explicitly finishing or terminating,
/// it is considered a "HangUp." The [`Receiver`] will be notified that
/// the sender is no longer available.
///
/// **Note:** It is strongly recommended to close the stream direction manually,
/// using `finish` or `terminate`.
pub struct Sender<S: Spec> {
    /// Sender to send actual data chunks.
    item_sender: weighted::Sender<Payload<S::Item>>,

    /// Sender to broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Receiver to listen for an external closure event (e.g., peer sends [Error::StopSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Cell to update stream priority dynamically.
    priority_cell: PriorityCell,
}

//noinspection DuplicatedCode
impl<S: Spec> Sender<S> {
    /// Sends a value, equivalent to [Payload::Chunk].
    ///
    /// See [`send_item`](Self::send_item).
    pub async fn send(&mut self, value: S::Item) -> Result<(), Error<S>> {
        self.send_item(Payload::Chunk(value)).await
    }

    /// Sends a value with `FIN` flag set, equivalent to [Payload::Last].
    ///
    /// See [`send_item`](Self::send_item).
    pub async fn send_final(mut self, value: S::Item) -> Result<(), Error<S>> {
        self.send_item(Payload::Last(value)).await
    }

    /// Sends a `FIN` flag, equivalent to [Payload::Done].
    ///
    /// See [`send_item`](Self::send_item).
    pub async fn finish(mut self) -> Result<(), Error<S>> {
        self.send_item(Payload::Done).await
    }

    /// Sends a manually crafted [Payload].
    ///
    /// If [Payload::is_fin], the channel will be automatically closed.
    ///
    /// **Note**: you will receive [Error::Finish] only on the attempt to send something
    /// after the channel is closed,
    /// therefore you will receive `Ok(())` on the initial request to close the channel.
    ///
    /// # Bounds
    /// The underlying channel is bounded, and uses [Estimate] trait for [S::Item]
    /// to understand how much each value weights in the channel.
    ///
    /// For example, with bound of `1024`, you can without blocking:
    /// - Buffer up to `1024` items with [`estimate`](Estimate) of `1`.
    /// - Or `infinite` number of items when [`estimate`](Estimate) returns `0` for each of them.
    /// - Or any other combination of items weight, until their sum exceeds the `bound`.
    ///  
    /// **Note**: with bound of `0`, the channel will block
    /// if there is more than `1` message in the internal queue,
    /// **unless** its weight is `0`.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The payload was sent and [`Receiver`] received it.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe.
    ///
    /// If `send` is used in `futures::select!` statement and some other branch completes first,
    /// then it is guaranteed that the message was not sent.
    ///
    /// However, in that case, the message is dropped and will be lost.
    pub async fn send_item(&mut self, item: Payload<S::Item>) -> Result<(), Error<S>> {
        if let Some(e) = self.error() {
            return Err(e);
        }

        let fin = item.is_fin();
        let weight = item.estimate();

        select_biased! {
            // Cancel safe:
            // - `error_receiver` is cancel_safe.
            // - `item_sender` is cancel_safe.

            err = self.error_receiver.recv().fuse() => {
                let err = err.unwrap_or_else(|| Self::fallback_error());
                Err(self.close(err))
            },
            result = self.item_sender.send(item, weight).fuse() => {
                if result.is_err() {
                    let err = Error::HangUp("Sender.item_receiver is unavailable".into());
                    return Err(self.close(err));
                }

                if fin {
                    let err = self.close(Error::Finish);

                    if !matches!(err, Error::Finish) {
                        return Err(err);
                    }
                }

                Ok(())
            },
        }
    }


    /// Abruptly terminates the stream direction with [Error::ResetSending].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub fn terminate(&mut self, error: S::Error) {
        self.close(Error::ResetSending(error));
    }

    /// Abruptly terminates the stream direction with [Error::Decoder].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_due_decoder(&mut self, error: <S::Decoder as Decoder>::Error) {
        self.close(Error::Decoder(error));
    }

    /// Abruptly terminates the stream direction with [Error::Connection].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_due_connection(&mut self, error: Arc<io::Error>) {
        self.close(Error::Connection(error));
    }

    /// Abruptly terminates the stream direction with [Error::HangUp].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns the terminal state of the stream, if known.
    ///
    /// Returns `None` if the stream direction is currently active and open.
    pub fn error(&self) -> Option<Error<S>> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns a handle to the error receiver.
    ///
    /// This allows external observers (like background tasks) to wait for the
    /// stream direction to complete or fail without holding a mutable reference to the sender.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error<S>> {
        self.error_receiver.clone()
    }


    /// Updates the priority of the stream direction.
    ///
    /// - `urgency`: Lower values indicate higher priority (0 is highest).
    /// - `incremental`:
    ///   Controls how bandwidth is shared among streams with the same urgency.
    ///
    ///    - `false`: **Sequential**. The scheduler attempts to send this stream's data
    ///      back-to-back until completion before moving to the next stream.
    ///      Use this for data that requires the whole payload to be useful (e.g., scripts, CSS).
    ///
    ///    - `true`: **Incremental**. The scheduler interleaves data from this stream
    ///      with other incremental streams of the same urgency.
    ///      Use this for data that can be processed as it arrives (e.g., progressive images, video).
    pub fn set_priority(&self, urgency: u8, incremental: bool) {
        self.priority_cell.set(Priority::new(urgency, incremental));
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.  
    fn close(&mut self, error: Error<S>) -> Error<S> {
        match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(SendError::Closed) => Self::fallback_error(),
            Err(SendError::Full) => self.error().expect("bug: there must be an error present"),
        }
    }

    fn fallback_error() -> Error<S> {
        Error::HangUp("Sender.error_receiver is unavailable".into())
    }
}

impl<S: Spec> Drop for Sender<S> {
    fn drop(&mut self) {
        let error: Error<S> = if panicking() {
            Error::HangUp("stream::Sender was dropped due to panic".into())
        } else {
            Error::HangUp(
                "stream::Sender was neither finished nor terminated manually, and dropped".into(),
            )
        };

        self.close(error);
    }
}


/// The receiving half of a QUIC stream.
///
/// # Lifecycle
///
/// 1. **Active**: Data is received using [`recv()`](Receiver::recv).
/// 2. **Finishing**: The stream direction is gracefully closed when the peer sends a `FIN` bit.
///    This is indicated by receiving [`Payload::Last`] or [`Payload::Done`].
/// 3. **Aborting**: The stream can be abruptly terminated locally using [`stop_sending()`](Receiver::stop_sending),
///    which sends a [Error::StopSending] frame to the peer.
///
/// # Drop Behavior
///
/// If the `Receiver` is dropped without the stream direction being finished or explicitly stopped,
/// it is considered a "HangUp." The [`Sender`] will be notified that
/// the receiver is no longer available.
///
/// **Note:** It is strongly recommended to handle the stream lifecycle gracefully
/// by consuming the data until a `FIN` is received or by explicitly calling `stop_sending`.
pub struct Receiver<S: Spec> {
    /// Receiver to receive actual data chunks.
    item_receiver: weighted::Receiver<Payload<S::Item>>,

    /// Sender to broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Receiver to listen for an external closure event (e.g., peer sends [Error::ResetSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Cell to get a dynamic priority from.
    priority_cell: PriorityCell,
}

//noinspection DuplicatedCode
impl<S: Spec> Receiver<S> {
    /// Receives the next chunk of data.
    ///
    /// **Note**: if [`Receiver`] receives error, such as [`Error::ResetSending`],
    /// all the messages will be ignored.
    ///
    /// # Return Value
    ///
    /// - `Ok(Payload)`: A chunk of data, the last chunk, or a completion signal.
    ///   If [Payload::Last] or [Payload::Done] is returned, the stream direction is considered closed,
    ///   and the **next** invocations will return [`Error::Finish`].
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe.
    /// If `recv` is used in `futures::select!` statement and some other branch completes first,
    /// it is guaranteed that no messages were received on this channel.
    pub async fn recv(&mut self) -> Result<Payload<S::Item>, Error<S>> {
        enum Event<E, I> {
            Err(Option<E>),
            Item(Option<I>),
        }

        let event = {
            let mut recv_error_fut = pin!(self.error_receiver.recv());
            let recv_error_fut = poll_fn(|cx| match recv_error_fut.as_mut().poll(cx) {
                // Ignore `Finish` errors, as it may lead to `item_receiver` future cancellation
                // and therefore a message loss.
                //
                // Handle `Finish` naturally from `item_receiver`.
                Poll::Ready(Some(Error::Finish)) => Poll::Pending,
                other => other,
            });

            select_biased! {
                // Cancel safe:
                // - `error_receiver` is cancel_safe.
                // - `item_receiver` is cancel_safe.
                
                err = recv_error_fut.fuse() => Event::Err(err),
                result = self.item_receiver.recv().fuse() => Event::Item(result),
            }
        };

        match event {
            Event::Err(err) => {
                let err = err.unwrap_or_else(|| Self::fallback_error());
                Err(self.close(err))
            }
            Event::Item(result) => match result {
                Some(item) => {
                    if item.is_fin() {
                        let err = self.close(Error::Finish);

                        if !matches!(err, Error::Finish) {
                            return Err(err);
                        }
                    }

                    Ok(item)
                }
                None => {
                    let err = Error::HangUp("Receiver.item_receiver is unavailable".into());
                    Err(self.close(err))
                }
            },
        }
    }

    /// Abruptly terminates the stream direction with [Error::StopSending].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub fn terminate(&mut self, error: S::Error) {
        self.close(Error::StopSending(error));
    }

    /// Abruptly terminates the stream direction with [Error::Encoder].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_due_encoder(&mut self, error: <S::Encoder as Encoder>::Error) {
        self.close(Error::Encoder(error));
    }

    /// Abruptly terminates the stream direction with [Error::Connection].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_due_connection(&mut self, error: Arc<io::Error>) {
        self.close(Error::Connection(error));
    }


    /// Abruptly terminates the stream direction with [Error::HangUp].
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns the terminal state of the stream, if known.
    ///
    /// Returns `None` if the stream direction is currently active and open.
    ///
    /// **Note**: there might be still data available in [Self::recv]
    /// if method returns [Error::Finish] and [Sender::send] was cancelled before.
    ///
    /// Call [Self::recv] if there is a need to know whether everything is consumed.
    pub fn error(&self) -> Option<Error<S>> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns a handle to the error receiver.
    ///
    /// This allows external observers (like background tasks) to wait for the
    /// stream direction to complete or fail without holding a mutable reference to the receiver.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error<S>> {
        self.error_receiver.clone()
    }


    /// Checks if the stream priority has changed and returns the new value if so.
    ///
    /// Returns `None` if the priority has not changed since the last call.
    pub(crate) fn priority_once(&mut self) -> Option<Priority> {
        self.priority_cell.take()
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.  
    fn close(&mut self, error: Error<S>) -> Error<S> {
        match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(SendError::Closed) => Self::fallback_error(),
            Err(SendError::Full) => self.error().expect("bug: there must be an error present"),
        }
    }

    fn fallback_error() -> Error<S> {
        Error::HangUp("Receiver.error_receiver is unavailable".into())
    }
}

impl<S: Spec> Drop for Receiver<S> {
    fn drop(&mut self) {
        let error: Error<S> = if panicking() {
            Error::HangUp("stream::Receiver was dropped due to panic".into())
        } else {
            Error::HangUp(
                "stream::Receiver was neither finished nor terminated manually, and dropped".into(),
            )
        };

        self.close(error);
    }
}


mod cell {
    use crate::conditional;
    use crate::stream::Priority;

    conditional! {
        multithread,

        use std::sync::Arc;
        use std::sync::atomic::AtomicU16;
        use std::sync::atomic::Ordering::AcqRel;
        use std::sync::atomic::Ordering::Release;

        const EMPTY: u16 = u16::MAX;

        pub struct PriorityCell {
            inner: Arc<AtomicU16>,
        }

        impl PriorityCell {
            pub fn new() -> Self {
                Self {
                    inner: Arc::new(AtomicU16::new(EMPTY))
                }
            }

            pub fn set(&self, priority: Priority) {
                self.inner.store(Self::pack(priority), Release);
            }

            pub fn take(&self) -> Option<Priority> {
                Self::unpack(self.inner.swap(EMPTY, AcqRel))
            }


            fn pack(priority: Priority) -> u16 {
                let incremental = priority.incremental as u8;
                u16::from_ne_bytes([priority.urgency, incremental])
            }

            fn unpack(value: u16) -> Option<Priority> {
                if value == EMPTY {
                    return None;
                }

                let [urgency, incremental] = value.to_ne_bytes();
                Some(Priority {
                    urgency,
                    incremental: incremental != 0,
                })
            }
        }
    }

    conditional! {
        not(multithread),

        use std::cell::Cell;
        use std::rc::Rc;

        pub struct PriorityCell {
            inner: Rc<Cell<Option<Priority>>>
        }

        impl PriorityCell {
            pub fn new() -> Self {
                Self {
                    inner: Rc::new(Cell::new(None))
                }
            }

            pub fn set(&self, priority: Priority) {
                self.inner.replace(Some(priority));
            }

            pub fn take(&self) -> Option<Priority> {
                self.inner.take()
            }
        }
    }

    impl Clone for PriorityCell {
        fn clone(&self) -> Self {
            Self {
                inner: self.inner.clone(),
            }
        }
    }
}
