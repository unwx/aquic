use crate::sync::stream;
use crate::{Estimate, Spec};
use aquic_macros::supports;
use bytes::Bytes;
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};


mod codec;
mod incoming;
mod outgoing;

pub use codec::*;
pub(crate) use incoming::*;
pub(crate) use outgoing::*;


/// A stream ID.
pub type StreamId = u64;

/// A new initiated stream.
pub enum Stream<S: Spec> {
    /// Client unidirectional stream.
    ClientUni(stream::Sender<S>),

    /// Client bidirectional stream
    ClientBidi(stream::Sender<S>, stream::Receiver<S>),

    /// Server unidirectional stream.
    ServerUni(stream::Receiver<S>),

    /// Server bidirectional stream.
    ServerBidi(stream::Sender<S>, stream::Receiver<S>),
}

impl<S: Spec> Debug for Stream<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClientUni(_) => f.debug_tuple("ClientUni").finish(),
            Self::ClientBidi(_, _) => f.debug_tuple("ClientBidi").finish(),
            Self::ServerUni(_) => f.debug_tuple("ServerUni").finish(),
            Self::ServerBidi(_, _) => f.debug_tuple("ServerBidi").finish(),
        }
    }
}


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
    Decoder(S::StreamDecoderError),

    /// Serialization of the outgoing message failed.
    Encoder(S::StreamEncoderError),

    /// A QUIC connection, this stream belongs to, is closed.
    Connection,

    /// The sending/receiving side unexpectedly disappeared.
    HangUp(Cow<'static, str>),
}

impl<S: Spec> Clone for Error<S> {
    fn clone(&self) -> Self {
        match self {
            Self::Finish => Self::Finish,
            Self::StopSending(e) => Self::StopSending(*e),
            Self::ResetSending(e) => Self::ResetSending(*e),
            Self::Decoder(e) => Self::Decoder(e.clone()),
            Self::Encoder(e) => Self::Encoder(e.clone()),
            Self::Connection => Self::Connection,
            Self::HangUp(e) => Self::HangUp(e.clone()),
        }
    }
}

impl<S: Spec> Debug for Error<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finish => write!(f, "Finish"),
            Self::StopSending(e) => f.debug_tuple("StopSending").field(e).finish(),
            Self::ResetSending(e) => f.debug_tuple("ResetSending").field(e).finish(),
            Self::Decoder(e) => f.debug_tuple("Decoder").field(e).finish(),
            Self::Encoder(e) => f.debug_tuple("Encoder").field(e).finish(),
            Self::Connection => f.debug_tuple("Connection").finish(),
            Self::HangUp(e) => f.debug_tuple("HangUp").field(e).finish(),
        }
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
            Self::Connection => write!(f, "connection is closed"),
            Self::HangUp(e) => write!(f, "hang-up ({e})"),
        }
    }
}

impl<S: Spec> std::error::Error for Error<S> {}


/// A chunk of data from a QUIC stream.
///
/// If `fin` is true, then no more chunks are going to arrive.
///
/// In unordered streams, `fin` flag may be set on intermediate chunk (chunk of bytes that was lost previously due to network conditions, etc).
/// But it would mean that all chunks were successfully received.
#[supports]
#[derive(Debug, Clone)]
pub enum Chunk {
    /// Chunk of data from an ordered QUIC stream.
    Ordered(Bytes),

    /// Chunk of data and a stream offset.
    #[supports(quinn)]
    Unordered(Bytes, u64),
}

/// Represents a piece of data received from a stream.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Payload<T> {
    /// An intermediate chunk of data.
    ///
    /// The stream direction remains open and more data is expected to follow.
    /// This corresponds to a frame with the `FIN` bit set to **0**.
    Item(T),

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
            Self::Item(val) | Self::Last(val) => Some(val),
            Self::Done => None,
        }
    }

    /// Maps the inner value to a new type, preserving the stream direction state.
    pub fn map<U, F>(self, f: F) -> Payload<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Self::Item(t) => Payload::Item(f(t)),
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
            Self::Item(t) => Payload::Item(f(t)?),
            Self::Last(t) => Payload::Last(f(t)?),
            Self::Done => Payload::Done,
        })
    }
}

impl<T: Estimate> Estimate for Payload<T> {
    fn estimate(&self) -> usize {
        match self {
            Payload::Item(it) => it.estimate(),
            Payload::Last(it) => it.estimate(),
            Payload::Done => 0,
        }
    }
}


/// A stream's priority that determines the order in which stream data is sent on the wire.
///
/// **Note** different [`QuicBackend`][crate::backend::QuicBackend] implement prioritization differently,
/// or don't implement at all.
#[supports]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Priority {
    /// Higher values indicate higher priority.
    #[supports(quiche, quinn)]
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
    ///
    #[supports(quiche)]
    pub incremental: bool,
}
