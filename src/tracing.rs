use crate::stream::StreamId;
use std::fmt::{Display, Formatter};
use tracing::{Span, Value, debug_span, field, info_span};

// Modifying Span key names is a breaking change.
// Modifying Display values is a breaking change.
// Adding keys/values is not a breaking change.

/// Creates a new [Span] for a QUIC I/O loop.
///
/// Each loop must have its own ID.
pub fn new_io_loop_span(id: u32) -> IoLoopSpan {
    IoLoopSpan(info_span!("quic_io_loop", io_loop_id = id))
}

/// Creates a new [Span] for a QUIC connection.
///
/// Connection belongs to an I/O loop, which span provided as argument.
pub fn new_connection_span<CId: Value>(parent: &IoLoopSpan, id: CId) -> ConnectionSpan {
    ConnectionSpan(debug_span!(
        parent: &parent.0,
        "quic_connection",
        connection_id = id,
        close_code = field::Empty,
        close_reason = field::Empty,
        close_type = field::Empty,
    ))
}

/// Creates a new [Span] for a QUIC stream.
///
/// Stream belongs to a connection, which span provided as argument.
pub fn new_stream_span(
    parent: &ConnectionSpan,
    id: StreamId,
    direction: StreamDirection,
    initiator: StreamInitiator,
) -> StreamSpan {
    StreamSpan(debug_span!(
        parent: &parent.0,
        "quic_stream",
        stream_id = id,
        direction = %direction,
        initiator = %initiator,
        close_code = field::Empty,
        close_reason = field::Empty,
    ))
}


/// A [Span] for a QUIC network I/O loop.
#[derive(Clone)]
pub struct IoLoopSpan(Span);

impl From<Span> for IoLoopSpan {
    fn from(span: Span) -> Self {
        IoLoopSpan(span)
    }
}

impl From<IoLoopSpan> for Span {
    fn from(span: IoLoopSpan) -> Self {
        span.0
    }
}


/// A [Span] for a QUIC connection.
#[derive(Clone)]
pub struct ConnectionSpan(Span);

impl ConnectionSpan {
    pub fn on_app_close(&self, code: u64, reason: &str) {
        self.0.record("close_code", code);
        self.0.record("close_reason", reason);
        self.0.record("close_type", "application");
    }

    pub fn on_transport_close(&self, code: u64, reason: &str) {
        self.0.record("close_code", code);
        self.0.record("close_reason", reason);
        self.0.record("close_type", "transport");
    }
}

impl From<Span> for ConnectionSpan {
    fn from(span: Span) -> Self {
        ConnectionSpan(span)
    }
}

impl From<ConnectionSpan> for Span {
    fn from(span: ConnectionSpan) -> Self {
        span.0
    }
}


/// A [Span] for a QUIC stream.
#[derive(Clone)]
pub struct StreamSpan(Span);

impl StreamSpan {
    pub fn on_reset_stream(&self, code: u64) {
        self.0.record("close_code", code);
        self.0.record("close_reason", "reset_stream");
    }

    pub fn on_stop_sending(&self, code: u64) {
        self.0.record("close_code", code);
        self.0.record("close_reason", "stop_sending");
    }

    pub fn on_connection_close(&self) {
        self.0.record("close_reason", "connection_close");
    }

    pub fn on_internal(&self) {
        self.0.record("close_reason", "internal");
    }

    pub fn on_fin(&self) {
        self.0.record("close_reason", "fin");
    }
}

impl From<Span> for StreamSpan {
    fn from(span: Span) -> Self {
        StreamSpan(span)
    }
}

impl From<StreamSpan> for Span {
    fn from(span: StreamSpan) -> Self {
        span.0
    }
}


#[derive(Debug, Copy, Clone)]
pub enum StreamDirection {
    In,
    Out,
}

impl Display for StreamDirection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::In => write!(f, "in"),
            Self::Out => write!(f, "out"),
        }
    }
}


#[derive(Debug, Copy, Clone)]
pub enum StreamInitiator {
    Local,
    Peer,
}

impl Display for StreamInitiator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Peer => write!(f, "peer"),
        }
    }
}
