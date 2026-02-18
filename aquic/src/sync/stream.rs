use crate::stream::{Error, Payload, Priority};
use crate::sync::TryRecvError;
use crate::sync::mpmc::oneshot::{self, OneshotReceiver, OneshotSender};
use crate::sync::mpsc::weighted;
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender};
use crate::sync::stream::cell::PriorityCell;
use crate::{Estimate, Spec};
use futures::{FutureExt, select_biased};
use std::any::type_name;
use std::borrow::Cow;
use std::future::poll_fn;
use std::pin::pin;
use std::task::Poll;
use std::thread::panicking;
use std::time::Instant;

/// Creates a `spsc` channel for a single stream direction communication.
///
/// This channel uses [Estimate][`crate::Estimate`] for each item.
///
/// The `bound` argument limits the weight of items that channel can hold.
/// It also may be `zero`, to hold no more than `1` item (unless its weight is `zero` too).
pub(crate) fn channel<S: Spec>(
    bound: usize,
    shutdown_warn_receiver: oneshot::Receiver<Instant>,
) -> (Sender<S>, Receiver<S>) {
    let (item_sender, item_receiver) = weighted::channel(bound);
    let (error_sender, error_receiver) = oneshot::channel();
    let priority_cell = PriorityCell::new();

    let stream_sender = Sender {
        item_sender,
        error_sender: error_sender.clone(),
        error_receiver: error_receiver.clone(),
        shutdown_warn_receiver: shutdown_warn_receiver.clone(),
        priority_cell: priority_cell.clone(),
    };
    let stream_receiver = Receiver {
        item_receiver,
        error_sender,
        error_receiver,
        shutdown_warn_receiver,
        priority_cell,
    };

    (stream_sender, stream_receiver)
}


/// The sending half of a QUIC stream.
///
/// # Lifecycle
///
/// - Active: Data is sent using [`send()`](Sender::send).
/// - Closed:
///     - Finish: The stream direction is gracefully closed using [`finish()`](Sender::finish)
///       or [`send_final()`](Sender::send_final), which sends a `FIN` bit to the peer.
///     - Aborted: The stream direction can be terminated using [`reset()`](Sender::reset),
///       sending a `RESET_STREAM` frame, or by receiving `STOP_SENDING` frame from a peer.
///
/// **Note**: by "sending" it's implied "scheduled to be sent by QUIC implementation and network I/O loop":
/// when you send `FIN` or `RESET_STREAM`, it is not guaranteed that stream is going to be finished with the specified state
/// on network level.
///
/// # Drop Behavior
///
/// If the `Sender` is dropped without explicitly finishing or terminating,
/// it is considered a "HangUp."
/// The [`Receiver`] will be notified that the sender is no longer available.
///
/// **Note:** It is recommended to close the stream direction manually,
/// using `finish` or `terminate`.
pub struct Sender<S: Spec> {
    /// Send actual data chunks.
    item_sender: weighted::Sender<Payload<S::StreamItem>>,

    /// Broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Listen for an external closure event (e.g., peer sends [Error::StopSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Listen for a warning, that connection is going to be closed at informed point of time.
    shutdown_warn_receiver: oneshot::Receiver<Instant>,

    /// Dynamic stream priority.
    priority_cell: PriorityCell,
}

impl<S: Spec> Sender<S> {
    /// Sends a value, equivalent to [Payload::Item].
    ///
    /// See [`send_item`](Self::send_item).
    pub async fn send(&mut self, value: S::StreamItem) -> Result<(), Error<S>> {
        self.send_item(Payload::Item(value)).await
    }

    /// Sends a value with `FIN` flag set, equivalent to [Payload::Last].
    ///
    /// See [`send_item`](Self::send_item).
    pub async fn send_final(mut self, value: S::StreamItem) -> Result<(), Error<S>> {
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
    ///
    /// The underlying channel is bounded and uses [Estimate] of the provided item,
    /// to see how much each item occupies the channel.
    ///
    /// Example:
    /// ```text
    /// item_1 weights 10.
    /// item_2 weights 15.
    /// item_3 weights 3.
    ///
    /// if sum(items) > bound: channel blocks until previous items are sent.
    /// ```
    ///
    /// **Notes**:
    /// - Item's weight may be `0`.
    /// - If `bound` is `0`, the channel will block if there is more than `1` item in the internal queue,
    /// **unless** its weight is `0`.
    ///
    /// # Returns
    ///
    /// - `Ok(())`: The item was enqueued for pick-up.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that the item will not be sent if the future drops.
    pub async fn send_item(&mut self, item: Payload<S::StreamItem>) -> Result<(), Error<S>> {
        let fin = item.is_fin();
        let weight = item.estimate();

        select_biased! {
            err = self.error_receiver.recv().fuse() => {
                let err = err.unwrap_or_else(|| Self::err_rx_fallback_error());
                Err(self.close(err))
            },
            result = self.item_sender.send(item, weight).fuse() => {
                if result.is_err() {
                    let err = Error::HangUp(format!("'{}.item_sender' is unavailable", type_name::<Self>()).into());
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


    /// Terminates direction with [Error::ResetSending].
    ///
    /// No-op if it's already closed.
    pub fn terminate(&mut self, error: S::Error) {
        self.close(Error::ResetSending(error));
    }

    /// Terminates direction with [Error::Decoder].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_decoder(&mut self, error: S::StreamDecoderError) {
        self.close(Error::Decoder(error));
    }

    /// Terminates direction with [Error::Connection].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_connection(&mut self) {
        self.close(Error::Connection);
    }

    /// Terminates direction with [Error::HangUp].
    ///
    /// No-op if it's already closed.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns error if direction is closed.
    pub fn error(&self) -> Option<Error<S>> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::err_rx_fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns an error receiver.
    ///
    /// This allows external observers to wait for the
    /// stream direction to complete without holding a mutable reference to the sender.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error<S>> {
        self.error_receiver.clone()
    }

    /// Returns a shutdown warning receiver.
    ///
    /// It's a receiver, used to listen for a warning,
    /// that connection is going to be closed at informed point of time.
    ///
    /// This allows external observers to wait for
    /// this signal and act accordingly to gracefully shutdown the stream, if possible.
    pub fn shutdown_warn_receiver(&self) -> oneshot::Receiver<Instant> {
        self.shutdown_warn_receiver.clone()
    }


    /// Updates the priority of the stream direction.
    pub fn set_priority(&self, priority: Priority) {
        self.priority_cell.set(priority);
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
            Err(oneshot::SendError::Closed) => Self::err_rx_fallback_error(),
            Err(oneshot::SendError::Full(actual_error)) => actual_error,
        }
    }

    fn err_rx_fallback_error() -> Error<S> {
        Error::HangUp(format!("'{}.error_receiver' is unavailable", type_name::<Self>()).into())
    }
}

impl<S: Spec> Drop for Sender<S> {
    fn drop(&mut self) {
        let error: Error<S> = if panicking() {
            Error::HangUp(format!("'{}' is dropped due to panic", type_name::<Self>()).into())
        } else {
            Error::HangUp(
                format!(
                    "'{}' is dropped without manual termination",
                    type_name::<Self>()
                )
                .into(),
            )
        };

        self.close(error);
    }
}


/// The receiving half of a QUIC stream.
///
/// # Lifecycle
///
/// - Active: Data is received using [`recv()`](Receiver::recv).
/// - Closed:
///     - Finish: The stream direction is gracefully closed when the peer sends a `FIN` bit.
///       This is indicated by receiving [`Payload::Last`] or [`Payload::Done`].
///     - Aborted: The stream can be terminated locally using [`stop_sending()`](Receiver::stop_sending),
///       which sends a `STOP_SENDING` frame to the peer, or by receiving `RESET_STREAM`.
///
/// # Drop Behavior
///
/// If the `Receiver` is dropped without the stream direction being finished or explicitly stopped,
/// it is considered a "HangUp."
/// The [`Sender`] will be notified that the receiver is no longer available.
///
/// **Note:** It is recommended to handle the stream lifecycle gracefully
/// by consuming the data until a `FIN` or error is received or by explicitly calling `stop_sending`.
pub struct Receiver<S: Spec> {
    /// Receive actual data chunks.
    item_receiver: weighted::Receiver<Payload<S::StreamItem>>,

    /// Broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Listen for an external closure event (e.g., peer sends [Error::ResetSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Listen for a warning, that connection is going to be closed at informed point of time.
    shutdown_warn_receiver: oneshot::Receiver<Instant>,

    /// Dynamic stream priority.
    priority_cell: PriorityCell,
}

impl<S: Spec> Receiver<S> {
    /// Receives the next chunk of data.
    ///
    /// **Note**: if [`Receiver`] receives error, such as [`Error::ResetSending`],
    /// all the messages will be ignored.
    ///
    /// # Returns
    ///
    /// - `Ok(Payload)`: A chunk of data, the last chunk, or a completion signal.
    ///   If [Payload::Last] or [Payload::Done] is returned, the stream direction is considered closed,
    ///   and the next invocations will return [`Error::Finish`].
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that no messages were received on this channel if future drops.
    pub async fn recv(&mut self) -> Result<Payload<S::StreamItem>, Error<S>> {
        enum Event<E, I> {
            Err(Option<E>),
            Item(Option<I>),
        }

        let event = {
            let mut recv_error_fut = pin!(self.error_receiver.recv());
            let recv_error_fut = poll_fn(|cx| match recv_error_fut.as_mut().poll(cx) {
                // Ignore `Finish` errors.
                //
                // Handle `Finish` naturally from `item_receiver`.
                Poll::Ready(Some(Error::Finish)) => Poll::Pending,
                other => other,
            });

            select_biased! {
                err = recv_error_fut.fuse() => Event::Err(err),
                result = self.item_receiver.recv().fuse() => Event::Item(result),
            }
        };

        match event {
            Event::Err(err) => {
                let err = err.unwrap_or_else(|| Self::err_rx_fallback_error());
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
                    let err = Self::item_rx_fallback_error();
                    Err(self.close(err))
                }
            },
        }
    }

    /// Try to receive a next chunk of data.
    ///
    /// More: [`recv()`][Receiver::recv].
    pub fn try_recv(&mut self) -> Result<Option<Payload<S::StreamItem>>, Error<S>> {
        match self.error_receiver.try_recv() {
            Ok(e) => {
                return Err(self.close(e));
            }
            Err(TryRecvError::Closed) => {
                return Err(self.close(Self::err_rx_fallback_error()));
            }
            Err(TryRecvError::Empty) => {
                // Do nothing, channel is open.
            }
        };

        match self.item_receiver.try_recv() {
            Ok(item) => {
                if item.is_fin() {
                    let err = self.close(Error::Finish);

                    if !matches!(err, Error::Finish) {
                        return Err(err);
                    }
                }

                Ok(Some(item))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Closed) => Err(self.close(Self::item_rx_fallback_error())),
        }
    }

    /// Terminates direction with [Error::StopSending].
    ///
    /// No-op if it's already closed.
    pub fn terminate(&mut self, error: S::Error) {
        self.close(Error::StopSending(error));
    }

    /// Terminates direction with [Error::Encoder].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_encoder(&mut self, error: S::StreamEncoderError) {
        self.close(Error::Encoder(error));
    }

    /// Terminates direction with [Error::Connection].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_connection(&mut self) {
        self.close(Error::Connection);
    }

    /// Terminates direction with [Error::HangUp].
    ///
    /// No-op if it's already closed.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns error if direction is closed.
    ///
    /// **Note**: there might be still data available in [Self::recv]
    /// if method returns [Error::Finish] and [Sender::send] was cancelled before.
    ///
    /// Call [Self::try_recv] if there is a need to know whether everything is consumed.
    pub fn error(&self) -> Option<Error<S>> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::err_rx_fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns an error receiver.
    ///
    /// This allows external observers to wait for the
    /// stream direction to complete without holding a mutable reference to the sender.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error<S>> {
        self.error_receiver.clone()
    }

    /// Returns a shutdown warning receiver.
    ///
    /// It's a receiver, used to listen for a warning,
    /// that connection is going to be closed at informed point of time.
    ///
    /// This allows external observers to wait for
    /// this signal and act accordingly to gracefully shutdown the stream, if possible.
    pub fn shutdown_warn_receiver(&self) -> oneshot::Receiver<Instant> {
        self.shutdown_warn_receiver.clone()
    }


    /// Checks if the stream priority has changed and returns `Some(new_val)` if so.
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
            Err(oneshot::SendError::Closed) => Self::err_rx_fallback_error(),
            Err(oneshot::SendError::Full(actual_error)) => actual_error,
        }
    }

    fn err_rx_fallback_error() -> Error<S> {
        Error::HangUp(format!("'{}.error_receiver' is unavailable", type_name::<Self>()).into())
    }

    fn item_rx_fallback_error() -> Error<S> {
        Error::HangUp(format!("'{}.item_receiver' is unavailable", type_name::<Self>()).into())
    }
}

impl<S: Spec> Drop for Receiver<S> {
    fn drop(&mut self) {
        let error: Error<S> = if panicking() {
            Error::HangUp(format!("'{}' is dropped due to panic", type_name::<Self>()).into())
        } else {
            Error::HangUp(
                format!(
                    "'{}' is dropped without manual termination",
                    type_name::<Self>()
                )
                .into(),
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
