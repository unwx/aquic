use crate::Spec;
use crate::stream::codec::{Decoder, Encoder};
use crate::stream::{Error, Payload, Priority};
use crate::sync::oneshot::{SendError, TryRecvError};
use crate::sync::{oneshot, rendezvous};
use std::borrow::Cow;
use std::io;
use std::sync::Arc;
use std::thread::panicking;
use tokio::select;
use tokio::sync::watch;

/// Creates the internal synchronization primitives for a single stream direction.
pub(crate) fn channel<S: Spec>() -> (Sender<S>, Receiver<S>) {
    let (item_sender, item_receiver) = rendezvous::channel();
    let (error_sender, error_receiver) = oneshot::channel();
    let (priority_sender, priority_receiver) = watch::channel(None);

    let stream_sender = Sender {
        item_sender,
        error_sender: error_sender.clone(),
        error_receiver: error_receiver.clone(),
        priority_sender,
    };
    let stream_receiver = Receiver {
        item_receiver,
        error_sender,
        error_receiver,
        priority_receiver,
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
    /// Channel to send actual data chunks.
    item_sender: rendezvous::Sender<Payload<S::Item>>,

    /// Oneshot channel to broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Receiver to listen for an external closure event (e.g., peer sends [Error::StopSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Channel to update stream priority dynamically.
    priority_sender: watch::Sender<Option<Priority>>,
}

//noinspection DuplicatedCode
impl<S: Spec> Sender<S> {
    /// Sends a chunk of data.
    ///
    /// To follow the peer's flow control, [`Sender`] uses a rendezvous channel internally.
    /// This method will wait until the [`Receiver`] receives the data.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The data was sent and [`Receiver`] received it.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will **not** lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    ///
    /// **Note**, still, if you [Sender::terminate] the stream,
    /// all existing messages will be ignored.
    pub async fn send(&mut self, value: S::Item) -> Result<(), Error<S>> {
        self.send_item(Payload::Chunk(value)).await
    }

    /// Sends the final chunk of data,
    /// and gracefully close the direction with a 'FIN' flag.
    ///
    /// To follow peer's flow control,
    /// [`Sender`] uses rendezvous channel inside:
    /// this method will wait until the [`Receiver`] receive the data.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The data was sent and [`Receiver`] received it.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will **not** lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    ///
    /// **Note**, still, if you [Sender::terminate] the stream,
    /// all existing messages will be ignored.
    pub async fn send_final(mut self, value: S::Item) -> Result<(), Error<S>> {
        self.send_item(Payload::Last(value)).await
    }

    /// Sends the final chunk of data,
    /// and gracefully close the direction with a 'FIN' flag.
    ///
    /// To follow peer's flow control,
    /// [`Sender`] uses rendezvous channel inside:
    /// this method will wait until the [`Receiver`] receive the signal.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The signal was sent and [`Receiver`] received it.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will **not** lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    ///
    /// **Note**, still, if you [Sender::terminate] the stream,
    /// all existing messages will be ignored.
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
    /// To follow peer's flow control,
    /// [`Sender`] uses rendezvous channel inside:
    /// this method will wait until the [`Receiver`] receive the payload.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The payload was sent and [`Receiver`] received it.
    /// - `Err(Error)`: The stream direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will **not** lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    ///
    /// **Note**, still, if you [Sender::terminate] the stream,
    /// all messages will be dropped instantly.
    pub async fn send_item(&mut self, item: Payload<S::Item>) -> Result<(), Error<S>> {
        if let Some(e) = self.error() {
            return Err(e);
        }

        let fin = item.is_fin();

        select! {
            biased;

            err = self.error_receiver.recv() => {
                let err = err.unwrap_or_else(|| Self::fallback_error());
                Err(self.close(err))
            },
            result = self.item_sender.send(item) => {
                if let Err(_) = result {
                    let err = Error::HangUp("Sender.error_receiver is unavailable".into());
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
            Err(TryRecvError::Disconnected) => Some(Self::fallback_error()),
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
        self.priority_sender
            .send_replace(Some(Priority::new(urgency, incremental)));
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.  
    fn close(&mut self, error: Error<S>) -> Error<S> {
        let error = match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(SendError::Closed) => Self::fallback_error(),
            Err(SendError::Full) => self.error().expect("bug: there must be an error present"),
        };

        self.item_sender.close();
        self.error_sender.close();
        self.error_receiver.close();
        error
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
    /// Channel to receive actual data chunks.
    item_receiver: rendezvous::Receiver<Payload<S::Item>>,

    /// Oneshot channel to broadcast the closure event of this stream direction.
    error_sender: oneshot::Sender<Error<S>>,

    /// Receiver to listen for an external closure event (e.g., peer sends [Error::ResetSending]).
    error_receiver: oneshot::Receiver<Error<S>>,

    /// Receiver to listen for priority updates from the sender.
    priority_receiver: watch::Receiver<Option<Priority>>,
}

//noinspection DuplicatedCode
impl<S: Spec> Receiver<S> {
    /// Receives the next chunk of data.
    ///
    /// To follow the peer's flow control, [`Receiver`] uses a rendezvous channel internally.
    /// This method participates in the handshake to notify the sender that the data has been received.
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
    /// This method is cancel-safe.
    ///
    /// If the future is dropped before completion,
    /// no data is lost and the rendezvous handshake is not completed for that item.
    pub async fn recv(&mut self) -> Result<Payload<S::Item>, Error<S>> {
        if let Some(err) = self.error() {
            return self.next_item_or_error(err).await;
        }

        select! {
            biased;

            err = self.error_receiver.recv() => {
                let mut err = err.unwrap_or_else(|| Self::fallback_error());
                err = self.close(err);

                self.next_item_or_error(err).await
            }
            result = self.item_receiver.recv() => {
                match result {
                    Some(item) => {
                        if item.is_fin() {
                            let err = self.close(Error::Finish);

                            if !matches!(err, Error::Finish) {
                                return Err(err);
                            }
                        }

                        Ok(item)
                    },
                    None => {
                        let err = Error::HangUp("Receiver.item_receiver is unavailable".into());
                        Err(self.close(err))
                    },
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
            Err(TryRecvError::Disconnected) => Some(Self::fallback_error()),
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
        if !self.priority_receiver.has_changed().unwrap_or(false) {
            return None;
        }

        self.priority_receiver.borrow_and_update().as_ref().copied()
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.  
    fn close(&mut self, error: Error<S>) -> Error<S> {
        let error = match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(SendError::Closed) => Self::fallback_error(),
            Err(SendError::Full) => self.error().expect("bug: there must be an error present"),
        };

        self.item_receiver.close();
        self.error_sender.close();
        self.error_receiver.close();
        error
    }

    async fn next_item_or_error(&mut self, err: Error<S>) -> Result<Payload<S::Item>, Error<S>> {
        #[allow(clippy::collapsible_if)] // If statement becomes too long.
        if matches!(err, Error::Finish) {
            if let Some(item) = self.item_receiver.recv().await {
                return Ok(item);
            }
        }

        Err(err)
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
