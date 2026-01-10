use crate::Spec;
use crate::stream::codec::{Decoder, Encoder};
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};
use std::io;
use std::sync::Arc;

pub mod codec;
mod incoming;
mod outgoing;
mod shared;

/// Represents the terminal, final state of a stream **direction**.
pub enum Error<S: Spec> {
    /// The stream direction completed successfully.
    ///
    /// The sending-side finished sending data (`FIN`).
    Finish,

    /// The stream direction was abruptly terminated by the receiving-side (`STOP_SENDING`).
    StopSending(S::Error),

    /// The stream direction was abruptly terminated by the sending-side (`RESET_STREAM`).
    ResetSending(S::Error),

    /// Deserialization of the incoming message failed.
    Decoder(<S::Decoder as Decoder>::Error),

    /// Serialization of the outgoing message failed.
    Encoder(<S::Encoder as Encoder>::Error),

    /// The underlying connection was severed unexpectedly.
    ///
    /// This represents an unrecoverable transport failure,
    /// such as a network timeout, connection loss.
    Connection(Arc<io::Error>),

    /// The sending/receiving side unexpectedly disappeared.
    HangUp(Cow<'static, str>),
}

impl<S: Spec> Clone for Error<S> {
    fn clone(&self) -> Self {
        match self {
            Self::Finish => Self::Finish,
            Self::StopSending(e) => Self::StopSending(e.clone()),
            Self::ResetSending(e) => Self::ResetSending(e.clone()),
            Self::Decoder(e) => Self::Decoder(e.clone()),
            Self::Encoder(e) => Self::Encoder(e.clone()),
            Self::Connection(e) => Self::Connection(e.clone()),
            Self::HangUp(e) => Self::HangUp(e.clone()),
        }
    }
}

impl<S: Spec> Debug for Error<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self, f)
    }
}

impl<S: Spec> Display for Error<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finish => write!(f, "finish"),
            Self::StopSending(e) => write!(f, "stop sending ({e})"),
            Self::ResetSending(e) => write!(f, "reset sending ({e})"),
            Self::Decoder(e) => write!(f, "decoder error ({e})"),
            Self::Encoder(e) => write!(f, "encoder error ({e})"),
            Self::Connection(e) => write!(f, "connection error ({e})"),
            Self::HangUp(e) => write!(f, "hang up ({e})"),
        }
    }
}

impl<S: Spec> std::error::Error for Error<S> {}


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
    pub fn is_fin(&self) -> bool {
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
    pub fn new(urgency: u8, incremental: bool) -> Self {
        Self {
            urgency,
            incremental,
        }
    }
}

/// A stream ID.
pub(crate) type StreamId = u64;
