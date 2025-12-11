use crate::ApplicationError;
use crate::sync::{oneshot, rendezvous};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use tokio::select;
use tokio::sync::watch;

/// Represents the terminal, final state of a stream direction.
#[derive(Debug, Clone)]
pub enum Outcome<E> {
    /// The stream direction completed successfully.
    ///
    /// * **For Receivers:** The peer finished sending data (sent `FIN`).
    /// * **For Senders:** The last message (with `FIN`) was successfully **buffered** for sending.
    Finish,

    /// The stream direction was abruptly terminated by the peer.
    ///
    /// * **For Receivers:** The peer sent a `RESET_STREAM` frame (aborting their send).
    /// * **For Senders:** The peer sent a `STOP_SENDING` frame (requesting we abort our send).
    ///
    /// This variant carries an application-defined error reason.
    Terminate(E),

    /// A failure occurred while encoding/decoding data.
    ///
    /// This indicates that the transport connection is healthy, but the payload
    /// could not be processed.
    ///
    /// * **For Receivers:** Deserialization of the incoming bytes failed.
    /// * **For Senders:** Serialization of the outgoing message failed.
    ///
    /// This variant carries an application-defined error reason,
    /// given by [`StreamEncoder`] or [`StreamDecoder`].
    ///
    /// [`StreamEncoder`]: crate::stream::codec::StreamEncoder
    /// [`StreamDecoder`]: crate::stream::codec::StreamDecoder
    Codec(E),

    /// The underlying connection was severed unexpectedly.
    ///
    /// This represents an unrecoverable transport failure, such as a network timeout,
    /// connection loss, or internal errors.
    ///
    /// The attached string is an optional diagnostic message or reason
    /// (e.g., "Connection Timed Out", "IO Error: Broken Pipe").
    HangUp(Cow<'static, str>),
}


impl<E: Display> Display for Outcome<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Finish => write!(f, "completed-successfully"),
            Outcome::Terminate(e) => write!(f, "terminated({e})"),
            Outcome::Codec(e) => write!(f, "codec-error({e})"),
            Outcome::HangUp(e) => write!(f, "hang-up({e})"),
        }
    }
}

/// Represents a piece of data received from a stream.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Payload<T> {
    /// An intermediate chunk of data.
    ///
    /// The stream direction remains open and more data is expected to follow.
    /// This corresponds to a frame with the `FIN` bit set to **0**.
    Chunk(T),

    /// The final chunk of data.
    ///
    /// This contains payload data, but also signals the end of the stream direction.
    /// No more data will arrive after this.
    /// This corresponds to a frame with data and the `FIN` bit set to **1**.
    Last(T),

    /// A termination signal with no data.
    ///
    /// The stream direction has closed successfully without delivering a final payload.
    /// This corresponds to an empty frame with the `FIN` bit set to **1**.
    Done,
}

impl<T> Payload<T> {
    /// Returns `true` if this item represents the end of the stream direction ([`Payload::Last`] or [`Payload::Done`]).
    pub fn is_finish(&self) -> bool {
        matches!(self, Self::Last(_) | Self::Done)
    }

    /// Returns the inner data if present, discarding the `FIN` state.
    pub fn into_inner(self) -> Option<T> {
        match self {
            Self::Chunk(val) | Self::Last(val) => Some(val),
            Self::Done => None,
        }
    }

    /// Maps the inner value to a new type, preserving the stream direction state.
    ///
    /// Useful for converting `Payload<Bytes>` to `Payload<String>`.
    pub fn map<U, F>(self, f: F) -> Payload<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Self::Chunk(t) => Payload::Chunk(f(t)),
            Self::Last(t) => Payload::Last(f(t)),
            Self::Done => Payload::Done,
        }
    }

    /// Maps the inner value to a new type, allowing the mapping function to return a Result.
    ///
    /// Useful for decoding bytes where the decoding might fail.
    pub fn try_map<U, E, F>(self, f: F) -> Result<Payload<U>, E>
    where
        F: FnOnce(T) -> Result<U, E>,
    {
        Ok(match self {
            Self::Chunk(t) => Payload::Chunk(f(t)?),
            Self::Last(t) => Payload::Last(f(t)?),
            Self::Done => Payload::Done,
        })
    }
}


/// A stream's priority that determines the order in which stream data is sent on the wire.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Priority {
    /// Lower values indicate higher priority (0 is highest).
    pub urgency: u8,

    /// Controls how bandwidth is shared among streams with the same urgency.
    ///
    /// - `false`: **Sequential**. The scheduler attempts to send this stream's data
    ///   back-to-back until completion before moving to the next stream.
    ///   Use this for data that requires the whole payload to be useful (e.g., scripts, CSS).
    ///
    /// - `true`: **Incremental**. The scheduler interleaves data from this stream
    ///   with other incremental streams of the same urgency.
    ///   Use this for data that can be processed as it arrives (e.g., progressive images, video).
    pub incremental: bool,
}

impl Priority {
    fn new(urgency: u8, incremental: bool) -> Self {
        Self {
            urgency,
            incremental,
        }
    }
}


/// Creates the internal synchronization primitives for a single stream direction.
///
/// This function initializes the necessary channels to bridge a [`Sender`]
/// and a [`Receiver`], allowing them to exchange:
///
/// * **Data**: Via a rendezvous channel to ensure flow control.
/// * **Lifecycle Events**: Via two-way oneshot channels to handle closures, `RESET_STREAM`, and `STOP_SENDING` signals.
/// * **Priority**: Via a watch channel for dynamic urgency updates.
///
/// # Returns
///
/// A tuple containing the connected sender and receiver pair.
pub(crate) fn channel<T, E: ApplicationError>() -> (Sender<T, E>, Receiver<T, E>) {
    let (item_sender, item_receiver) = rendezvous::channel();
    let (to_receiver_outcome_sender, from_sender_outcome_receiver) = oneshot::channel();
    let (to_sender_outcome_sender, from_receiver_outcome_receiver) = oneshot::channel();
    let (priority_sender, priority_receiver) = watch::channel(None);

    let stream_sender = Sender {
        item_sender,
        outcome_sender: to_receiver_outcome_sender,
        outcome_receiver: from_receiver_outcome_receiver,
        priority_sender,
        received_outcome: None,
    };
    let stream_receiver = Receiver {
        item_receiver,
        outcome_sender: to_sender_outcome_sender,
        outcome_receiver: from_sender_outcome_receiver,
        priority_receiver,
        received_outcome: None,
    };

    (stream_sender, stream_receiver)
}


/// The sending half of a QUIC stream.
///
///
/// # Lifecycle
///
/// 1. **Active**: Data is sent using [`send()`](Sender::send).
/// 2. **Finishing**: The stream direction is gracefully closed using [`finish()`](Sender::finish) or [`send_final()`](Sender::send_final).
///    This sends a `FIN` bit to the peer.
/// 3. **Aborting**: The stream direction can be abruptly terminated using [`reset()`](Sender::reset), sending a `RESET_STREAM` frame.
///
/// # Drop Behavior
///
/// If the `Sender` is dropped without explicitly finishing or resetting,
/// it is considered a "HangUp." The [`Receiver`] will be notified that
/// the sender is no longer available.
///
/// **Note:** It is strongly recommended to close the stream direction gracefully whenever possible.
pub struct Sender<T, E> {
    /// Channel to send actual data chunks.
    item_sender: rendezvous::Sender<Payload<T>>,

    /// Oneshot channel to broadcast the final result of this stream direction.
    outcome_sender: oneshot::Sender<Outcome<E>>,

    /// Receiver to listen for an external closure event (e.g., peer sends `STOP_SENDING`).
    outcome_receiver: oneshot::Receiver<Outcome<E>>,

    /// Channel to update stream priority dynamically.
    priority_sender: watch::Sender<Option<Priority>>,

    /// Received final outcome. **Once set, the stream direction is considered closed**.
    received_outcome: Option<Outcome<E>>,
}

impl<T, E: ApplicationError> Sender<T, E> {
    /// Sends a chunk of data.
    ///
    /// To follow the peer's control flow, [`Sender`] uses a rendezvous channel internally.
    /// This method will wait until the [`Receiver`] receives the data.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The data was sent and [`Receiver`] received it.
    /// - `Err(Outcome)`: The stream direction is closed (e.g., peer requested `STOP_SENDING`, connection died, etc.).
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will not lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    pub async fn send(&mut self, value: T) -> Result<(), Outcome<E>> {
        self.send_item(Payload::Chunk(value)).await
    }

    /// Sends the final chunk of data,
    /// and gracefully close the direction with a 'FIN' flag.
    ///
    /// To follow peer's control flow,
    /// [`Sender`] uses rendezvous channel inside:
    /// this method will wait until the [`Receiver`] receive the data.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The data was sent and [`Receiver`] received it.
    /// - `Err(Outcome)`: The stream direction is closed (e.g., peer requested `STOP_SENDING`,
    ///   connection died, or local side already finished).
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will not lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    pub async fn send_final(mut self, value: T) -> Result<(), Outcome<E>> {
        self.send_item(Payload::Last(value)).await
    }

    /// Sends the final chunk of data,
    /// and gracefully close the direction with a 'FIN' flag.
    ///
    /// To follow peer's control flow,
    /// [`Sender`] uses rendezvous channel inside:
    /// this method will wait until the [`Receiver`] receive the signal.
    ///
    /// # Return Value
    ///
    /// - `Ok(())`: The signal was sent and [`Receiver`] received it.
    /// - `Err(Outcome)`: The stream direction is closed (e.g., peer requested `STOP_SENDING`,
    ///   connection died, or local side already finished).
    ///
    /// # Cancel Safety
    ///
    /// As this method uses rendezvous handshake,
    /// canceling this future breaks the strict rendezvous guarantee,
    /// but will not lose the message.
    ///
    /// More details: [`rendezvous::Sender::send()`].
    pub async fn finish(mut self) -> Result<(), Outcome<E>> {
        self.send_item(Payload::Done).await
    }

    async fn send_item(&mut self, item: Payload<T>) -> Result<(), Outcome<E>> {
        if let Some(outcome) = &self.received_outcome {
            return Err(outcome.clone());
        }

        select! {
            biased;

            maybe_outcome = self.outcome_receiver.recv() => {
                let outcome = maybe_outcome.unwrap_or_else(
                    || Outcome::HangUp("stream::Receiver became unavailable".into())
                );
                self.close(outcome.clone());
                Err(outcome)
            },
            delivered = self.item_sender.send(item) => {
                if delivered {
                    Ok(())
                } else {
                    let outcome = Outcome::HangUp("stream::Receiver became unavailable".into());
                    self.close(outcome.clone());
                    Err(outcome)
                }
            },
        }
    }


    /// Abruptly terminates the stream direction with an application-specific error code.
    ///
    /// This triggers the transmission of a `RESET_STREAM` frame to the peer.
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub fn terminate(self, code: E) {
        if self.received_outcome.is_none() {
            let _ = self.outcome_sender.send(Outcome::Terminate(code));
        }
    }

    //noinspection DuplicatedCode
    /// Closes the receiver with a codec error.
    ///
    /// This is used internally when encoding fails.
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_codec(self, code: E) {
        if self.received_outcome.is_none() {
            let _ = self.outcome_sender.send(Outcome::Codec(code));
        }
    }


    /// Returns the terminal state of the stream, if known.
    ///
    /// Returns `None` if the stream direction is currently active and open.
    pub fn outcome(&self) -> Option<Outcome<E>> {
        self.received_outcome.clone()
    }

    /// Returns a handle to the outcome receiver.
    ///
    /// This allows external observers (like background tasks) to wait for the
    /// stream direction to complete or fail without holding a mutable reference to the sender.
    pub fn outcome_receiver(&self) -> oneshot::Receiver<Outcome<E>> {
        self.outcome_receiver.clone()
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
}

impl<T, E> Sender<T, E> {
    fn close(&mut self, outcome: Outcome<E>) {
        self.received_outcome = Some(outcome);
        self.item_sender.close();
        self.outcome_sender.close();
        self.outcome_receiver.close();
    }
}

impl<T, E> Drop for Sender<T, E> {
    fn drop(&mut self) {
        if self.received_outcome.is_none() {
            // Send HangUp if the direction is not closed yet (received_outcome.is_none()),
            // and Receiver hasn't received any signal before.
            let _ = self.outcome_sender.send(Outcome::HangUp(
                "stream::Sender became unavailable".into(),
            ));

            self.item_sender.close();
            self.outcome_sender.close();
            self.outcome_receiver.close();
        }
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
///    which sends a `STOP_SENDING` frame to the peer.
///
/// # Drop Behavior
///
/// If the `Receiver` is dropped without the stream direction being finished or explicitly stopped,
/// it is considered a "HangUp." The [`Sender`] will be notified that
/// the receiver is no longer available.
///
/// **Note:** It is strongly recommended to handle the stream lifecycle gracefully
/// by consuming the data until a `FIN` is received or by explicitly calling `stop_sending`.
pub struct Receiver<T, E> {
    /// Channel to receive actual data chunks.
    item_receiver: rendezvous::Receiver<Payload<T>>,

    /// Oneshot channel to broadcast the final result of this stream direction.
    outcome_sender: oneshot::Sender<Outcome<E>>,

    /// Receiver to listen for an external closure event (e.g., peer sends `RESET_STREAM`).
    outcome_receiver: oneshot::Receiver<Outcome<E>>,

    /// Receiver to listen for priority updates from the sender.
    priority_receiver: watch::Receiver<Option<Priority>>,

    /// Received final outcome. **Once set, the stream direction is considered closed**.
    received_outcome: Option<Outcome<E>>,
}

impl<T, E: ApplicationError> Receiver<T, E> {
    /// Receives the next chunk of data.
    ///
    /// To follow the peer's control flow, [`Receiver`] uses a rendezvous channel internally.
    /// This method participates in the handshake to notify the sender that the data has been received.
    ///
    /// # Return Value
    ///
    /// - `Ok(Payload)`: A chunk of data, the last chunk, or a completion signal.
    ///   If `Payload::Last` or `Payload::Done` is returned, the stream direction is considered closed,
    ///   and the next invocations will return [`Outcome::Finish`].
    /// - `Err(Outcome)`: The stream direction is closed (e.g., peer sent `RESET_STREAM`, connection died, etc.).
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel-safe.
    ///
    /// If the future is dropped before completion,
    /// no data is lost and the rendezvous handshake is not completed for that item.
    pub async fn recv(&mut self) -> Result<Payload<T>, Outcome<E>> {
        if let Some(outcome) = &self.received_outcome {
            return Err(outcome.clone());
        }

        let result = select! {
            biased;

            maybe_outcome = self.outcome_receiver.recv() => {
                Err(maybe_outcome.unwrap_or_else(
                    || Outcome::HangUp("stream::Sender became unavailable".into())
                ))
            },
            result = self.item_receiver.recv() => {
                result.ok_or_else(
                    || Outcome::HangUp("stream::Sender became unavailable".into())
                )
            },
        };

        match &result {
            Ok(item) => {
                if item.is_finish() {
                    self.close(Outcome::Finish);
                }
            }
            Err(outcome) => {
                self.close(outcome.clone());
            }
        }

        result
    }

    /// Abruptly requests the peer to stop sending data with an application-specific error code.
    ///
    /// This triggers the transmission of a `STOP_SENDING` frame to the peer.
    ///
    /// If the direction is already closed, this operation is a no-op.
    pub fn terminate(self, code: E) {
        if self.received_outcome.is_none() {
            let _ = self.outcome_sender.send(Outcome::Terminate(code));
        }
    }

    //noinspection DuplicatedCode
    /// Closes the receiver with a codec error.
    ///
    /// This is used internally when decoding fails.
    /// If the direction is already closed, this operation is a no-op.
    pub(crate) fn terminate_codec(self, code: E) {
        if self.received_outcome.is_none() {
            let _ = self.outcome_sender.send(Outcome::Codec(code));
        }
    }


    /// Returns the terminal state of the stream, if known.
    ///
    /// Returns `None` if the stream is currently active and open.
    pub fn outcome(&self) -> Option<Outcome<E>> {
        self.received_outcome.clone()
    }

    /// Returns a handle to the outcome receiver.
    ///
    /// This allows external observers (like background tasks) to wait for the
    /// stream direction to complete or fail without holding a mutable reference to the receiver.
    pub fn outcome_receiver(&self) -> oneshot::Receiver<Outcome<E>> {
        self.outcome_receiver.clone()
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
}

impl<T, E> Receiver<T, E> {
    fn close(&mut self, outcome: Outcome<E>) {
        self.received_outcome = Some(outcome);
        self.item_receiver.close();
        self.outcome_sender.close();
        self.outcome_receiver.close();
    }
}

impl<T, E> Drop for Receiver<T, E> {
    fn drop(&mut self) {
        if self.received_outcome.is_none() {
            // Send HangUp if the direction is not closed yet (received_outcome.is_none()),
            // and Sender hasn't received any signal before.
            let _ = self.outcome_sender.send(Outcome::HangUp(
                "stream::Receiver became unavailable".into(),
            ));

            self.item_receiver.close();
            self.outcome_sender.close();
            self.outcome_receiver.close();
        }
    }
}
