use std::{
    borrow::Cow,
    fmt::{Debug, Display, Formatter},
};

use bytes::Bytes;

use crate::{exec::SendOnMt, stream::StreamId};

/// [QuicBackend] result.
pub type Result<T> = std::result::Result<T, Error>;

/// [QuicBackend] error.
#[derive(Debug, Clone)]
pub enum Error {
    /// Backend is closed.
    Closed,

    /// Illegal operation.
    ///
    /// For example, on attempt to create a new client connection,
    /// while backend is working on server-only mode.
    Illegal,

    /// Connection with provided `connection_id` cannot be found.
    UnknownConnection,

    /// Stream with provided `stream_id` cannot be found.
    UnknownStream,

    /// A refused attempt to open a new stream with a specific direction.
    ///
    /// The streams in the given direction are currently exhausted.
    StreamsExhausted,

    /// Unable to perform operation on the stream: it's finished.
    StreamFinish,

    /// Unable to perform operation on the stream: it's terminated with `STOP_SENDING(err)`.
    StreamStopSending(u64),

    /// Unable to perform operation on the stream: it's terminated with `RESET_STREAM(err)`.
    StreamResetSending(u64),

    /// Other uncovered, yet non-fatal error.
    Other(Cow<'static, str>),

    /// Fatal error, QUIC backend is no more responsive and should be considered closed.
    Fatal(Cow<'static, str>),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Closed => write!(f, "backend is closed"),
            Error::Illegal => write!(f, "illegal operation"),
            Error::UnknownConnection => write!(f, "unknown connection"),
            Error::UnknownStream => write!(f, "unknown stream"),
            Error::StreamsExhausted => write!(f, "streams are exhausted"),
            Error::StreamFinish => write!(f, "stream is finished"),
            Error::StreamStopSending(e) => {
                write!(f, "stream is terminated with 'STOP_SENDING({e})'")
            }
            Error::StreamResetSending(e) => {
                write!(f, "stream is terminated with 'RESET_STREAM({e})'")
            }
            Error::Other(e) => write!(f, "other error: {e}"),
            Error::Fatal(e) => write!(f, "fatal error: {e}"),
        }
    }
}

impl std::error::Error for Error {}


/// QUIC connection ID may change from time to time for a single connection.
///
/// Stable connection ID will never change.
pub trait StableConnectionId: Debug + Clone + SendOnMt + Unpin + 'static {}
impl<T: Debug + Clone + SendOnMt + Unpin + 'static> StableConnectionId for T {}


/// Appendable connection/stream events.
pub struct OutEvents<'a, CId: StableConnectionId> {
    connection_events: &'a mut Vec<ConnectionEvent<CId>>,
    stream_events: &'a mut Vec<StreamEvent<CId>>,

    /// When I/O loop decides to close QUIC backend,
    /// there might be a huge burst of `close` events for both: connections and streams.
    ///
    /// To prevent this memory burst and its potential harmful consequences,
    /// we will just ignore any event.
    immutable: bool,
    modified: bool,
}

impl<'a, CId: StableConnectionId> OutEvents<'a, CId> {
    #[inline]
    pub fn new(
        connection_events: &'a mut Vec<ConnectionEvent<CId>>,
        stream_events: &'a mut Vec<StreamEvent<CId>>,
    ) -> Self {
        Self {
            connection_events,
            stream_events,
            immutable: false,
            modified: false,
        }
    }

    #[inline]
    pub fn new_noop(
        connection_events: &'a mut Vec<ConnectionEvent<CId>>,
        stream_events: &'a mut Vec<StreamEvent<CId>>,
    ) -> Self {
        Self {
            connection_events,
            stream_events,
            immutable: true,
            modified: false,
        }
    }

    /// Returns `true` if there are new events available.
    #[inline]
    pub fn is_modified(&self) -> bool {
        self.modified
    }


    /// See [Vec::push].
    #[inline]
    pub fn push_connection_event(&mut self, event: ConnectionEvent<CId>) {
        self.do_if_mut(|it| it.connection_events.push(event));
    }

    /// See [Vec::push].
    #[inline]
    pub fn push_stream_event(&mut self, event: StreamEvent<CId>) {
        self.do_if_mut(|it| it.stream_events.push(event));
    }


    ///See [Vec::append].
    #[inline]
    pub fn append_connection_events(&mut self, events: &mut Vec<ConnectionEvent<CId>>) {
        self.do_if_mut(|it| it.connection_events.append(events));
    }

    ///See [Vec::append].
    #[inline]
    pub fn append_stream_events(&mut self, events: &mut Vec<StreamEvent<CId>>) {
        self.do_if_mut(|it| it.stream_events.append(events));
    }


    ///See [Extend].
    #[inline]
    pub fn extend_connection_events<I: IntoIterator<Item = ConnectionEvent<CId>>>(
        &mut self,
        events: I,
    ) {
        self.do_if_mut(|it| it.connection_events.extend(events));
    }

    ///See [Extend].
    #[inline]
    pub fn extend_stream_events<I: IntoIterator<Item = StreamEvent<CId>>>(&mut self, events: I) {
        self.do_if_mut(|it| it.stream_events.extend(events));
    }


    #[inline(always)]
    fn do_if_mut<F>(&mut self, func: F)
    where
        F: FnOnce(&mut OutEvents<'a, CId>),
    {
        if self.immutable {
            return;
        } else {
            self.modified = true;
        }

        func(self);
    }
}


/// A QUIC connection related event.
#[derive(Debug, Clone)]
pub enum ConnectionEvent<CId: StableConnectionId> {
    /// Blank connection is created.
    ///
    /// It may be just exist in memory, or on a handshake stage.
    Created(CId),

    /// There is handshake data available,
    /// but connection may be still not ready for communication.
    HandshakeDataReady(CId),

    /// Connection is established.
    Active(CId),

    /// Connection is draining or closed
    /// with an application or transport error.
    Closed {
        /// Connection ID.
        connection_id: CId,

        /// Code the connection was closed with.
        code: u64,

        /// Close reason in UTF-8.
        reason: Bytes,

        /// Is this a transport, or application close kind.
        is_transport: bool,

        /// Is the initiator our application, or the peer.
        is_local: bool,
    },

    /// Connection is closed due to QUIC version mismatch.
    ClosedVersionMismatch(CId),

    /// Connection is closed due to timeout.
    ClosedTimeout(CId),

    /// Connection is closed due to QUIC version mismatch.
    ClosedUnknown {
        /// Connection ID.
        connection_id: CId,

        /// Close reason in UTF-8.
        reason: Bytes,
    },
}

impl<CId: StableConnectionId> ConnectionEvent<CId> {
    pub fn connection_id(&self) -> CId {
        match self {
            Self::Created(id) => id.clone(),
            Self::HandshakeDataReady(id) => id.clone(),
            Self::Active(id) => id.clone(),
            Self::Closed { connection_id, .. } => connection_id.clone(),
            Self::ClosedVersionMismatch(id) => id.clone(),
            Self::ClosedTimeout(id) => id.clone(),
            Self::ClosedUnknown { connection_id, .. } => connection_id.clone(),
        }
    }

    /// Returns an order in which events should generally appear.
    pub fn order(&self) -> u8 {
        match self {
            Self::Created(_) => 0,
            Self::HandshakeDataReady(_) => 1,
            Self::Active(_) => 2,
            Self::Closed { .. } => 3,
            Self::ClosedVersionMismatch(_) => 3,
            Self::ClosedTimeout(_) => 3,
            Self::ClosedUnknown { .. } => 3,
        }
    }
}


/// A QUIC stream related event.
#[derive(Debug, Copy, Clone)]
pub enum StreamEvent<CId: StableConnectionId> {
    /// Peer opened a new stream.
    Open {
        connection_id: CId,
        stream_id: StreamId,

        /// Whether the stream is bidirectional or read-only.
        bidirectional: bool,
    },

    /// Application may open a new stream of certain directionality.
    Available {
        connection_id: CId,

        /// Whether the stream is bidirectional or write-only.
        bidirectional: bool,
    },

    /// Incoming(read) stream direction has new data to read after a break.
    ///
    /// Please don't create this event on every new stream data frame, if possible.
    /// It's enough to create this stream once after application read all of the pending data on the stream,
    /// and new data has been received.
    InActive(CId, StreamId),

    /// Outgoing(write) stream direction has capacity for new data after a break.
    ///
    /// Please don't create this event on every new stream data frame, if possible.
    /// It's enough to create this stream once after application faced the stream flow control limit,
    /// and the stream is available for new data again.
    OutActive(CId, StreamId),

    /// Incoming(read) stream direction is terminated with `RESET_STREAM(u64)`.
    InReset(CId, StreamId, u64),

    /// Incoming(read) stream direction is terminated with `STOP_SENDING(u64)`.
    OutReset(CId, StreamId, u64),
}
