use crate::stream::StreamId;
use std::fmt::{Display, Formatter};
use tracing::{Span, Value, debug_span, field, info_span};

// Modifying Span key names is a breaking change.
// Modifying Display values is a breaking change.
// Adding keys/values is not a breaking change.


/// A [Span] for a QUIC network I/O loop.
#[derive(Clone)]
pub(crate) struct IoLoopSpan(Span);

impl IoLoopSpan {
    /// Creates a new [Span] for a QUIC I/O loop.
    ///
    /// Each loop must have its own ID.
    #[inline]
    pub fn new(id: u32) -> IoLoopSpan {
        IoLoopSpan(info_span!("quic_io_loop", io_loop_id = id))
    }
}

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
pub(crate) struct ConnectionSpan(Span);

impl ConnectionSpan {
    const CLOSE_CODE: &'static str = "close_code";
    const CLOSE_REASON: &'static str = "close_reason";
    const CLOSE_KIND: &'static str = "close_kind";
    const CLOSE_INITIATOR: &'static str = "close_initiator";

    /// Creates a new [Span] for a QUIC connection.
    ///
    /// Connection belongs to an I/O loop, which span provided as argument.
    #[inline]
    pub fn new<CId: Value>(parent: &IoLoopSpan, id: CId, initiator: Initiator) -> Self {
        ConnectionSpan(debug_span!(
            parent: &parent.0,
            "quic_connection",
            connection_id = id,
            initiator = %initiator,
            close_code = field::Empty,
            close_reason = field::Empty,
            close_kind = field::Empty,
            close_initiator = field::Empty,
        ))
    }

    #[inline]
    #[rustfmt::skip]
    pub fn on_app_close(&self, code: u64, reason: &str, initiator: Initiator) {
        self.0.record(Self::CLOSE_CODE, code);
        self.0.record(Self::CLOSE_REASON, reason);
        self.0.record(Self::CLOSE_INITIATOR, field::display(initiator));
        self.0.record(Self::CLOSE_KIND, "application");
    }

    #[inline]
    #[rustfmt::skip]
    pub fn on_transport_close(&self, code: u64, reason: &str, initiator: Initiator) {
        self.0.record(Self::CLOSE_CODE, code);
        self.0.record(Self::CLOSE_REASON, reason);
        self.0.record(Self::CLOSE_INITIATOR, field::display(initiator));
        self.0.record(Self::CLOSE_KIND, "transport");
    }

    #[inline]
    #[rustfmt::skip]
    pub fn on_unknown_close(&self, reason: &str) {
        self.0.record(Self::CLOSE_REASON, reason);
        self.0.record(Self::CLOSE_INITIATOR, field::display(Initiator::Local));
        self.0.record(Self::CLOSE_KIND, "unknown");
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


/// A [Span] for a QUIC datagram flow.
#[derive(Clone)]
pub(crate) struct DgramSpan(Span);

impl DgramSpan {
    const CLOSE_KIND: &'static str = "close_kind";

    /// Creates a new [Span] for a QUIC datagram direction.
    ///
    /// It belongs to a connection, which span provided as argument.
    #[inline]
    pub fn new<CId: Value>(parent: &ConnectionSpan, id: CId, direction: Direction) -> Self {
        DgramSpan(debug_span!(
            parent: &parent.0,
            "quic_dgram",
            connection_id = id,
            direction = %direction,
            close_kind = field::Empty,
        ))
    }

    #[inline]
    pub fn on_connection_close(&self) {
        self.0.record(Self::CLOSE_KIND, "connection_close");
    }

    #[inline]
    pub fn on_internal(&self) {
        self.0.record(Self::CLOSE_KIND, "internal");
    }

    #[inline]
    pub fn on_normal_end(&self) {
        self.0.record(Self::CLOSE_KIND, "end");
    }
}

impl From<Span> for DgramSpan {
    fn from(span: Span) -> Self {
        DgramSpan(span)
    }
}

impl From<DgramSpan> for Span {
    fn from(span: DgramSpan) -> Self {
        span.0
    }
}


/// A [Span] for a QUIC stream.
#[derive(Clone)]
pub(crate) struct StreamSpan(Span);

impl StreamSpan {
    const CLOSE_CODE: &'static str = "close_code";
    const CLOSE_KIND: &'static str = "close_kind";

    /// Creates a new [Span] for a QUIC stream.
    ///
    /// Stream belongs to a connection, which span provided as argument.
    #[inline]
    pub fn new(
        parent: &ConnectionSpan,
        id: StreamId,
        direction: Direction,
        initiator: Initiator,
    ) -> Self {
        StreamSpan(debug_span!(
            parent: &parent.0,
            "quic_stream",
            stream_id = id,
            direction = %direction,
            initiator = %initiator,
            close_code = field::Empty,
            close_kind = field::Empty,
        ))
    }

    #[inline]
    pub fn on_reset_stream(&self, code: u64) {
        self.0.record(Self::CLOSE_CODE, code);
        self.0.record(Self::CLOSE_KIND, "reset_stream");
    }

    #[inline]
    pub fn on_stop_sending(&self, code: u64) {
        self.0.record(Self::CLOSE_CODE, code);
        self.0.record(Self::CLOSE_KIND, "stop_sending");
    }

    #[inline]
    pub fn on_connection_close(&self) {
        self.0.record(Self::CLOSE_KIND, "connection_close");
    }

    #[inline]
    pub fn on_internal(&self) {
        self.0.record(Self::CLOSE_KIND, "internal");
    }

    #[inline]
    pub fn on_fin(&self) {
        self.0.record(Self::CLOSE_KIND, "fin");
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
pub(crate) enum Direction {
    In,
    Out,
}

impl Display for Direction {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::In => write!(f, "in"),
            Self::Out => write!(f, "out"),
        }
    }
}


#[derive(Debug, Copy, Clone)]
pub(crate) enum Initiator {
    Local,
    Peer,
}

impl Display for Initiator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Peer => write!(f, "peer"),
        }
    }
}
