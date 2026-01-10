use crate::stream::{Priority, StreamId};
use bytes::{Bytes, BytesMut};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use tracing::debug;

/// An error during a QUIC stream operation.
#[derive(Debug, Clone)]
pub(crate) enum StreamError {
    /// The stream direction is already finished.
    Finish,

    /// The stream direction was terminated with a `STOP_SENDING` frame.
    StopSending(u64),

    /// The stream direction was terminated with a `RESET_STREAM` frame.
    ResetSending(u64),

    /// Other backend error, that is not covered above.
    Other(Cow<'static, str>),
}

impl Display for StreamError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finish => write!(f, "finished"),
            Self::StopSending(e) => write!(f, "stop sending ({e})"),
            Self::ResetSending(e) => write!(f, "reset sending ({e})"),
            Self::Other(e) => write!(f, "other ({e})"),
        }
    }
}

impl std::error::Error for StreamError {}

/// A QUIC stream operation result.
pub(crate) type StreamResult<T> = Result<T, StreamError>;


/// A QUIC stream API for various implementations of the QUIC protocol.
pub(crate) trait StreamBackend {
    /// Read available stream data into the buffer.
    ///
    /// Returns number of bytes written into the buffer,
    /// and whether it is a successful end of the stream direction (`FIN`).
    ///
    /// `FIN` might be returned only once, all further calls will return [StreamError::Finish].
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to read from client-unidirectional (or write only) stream.
    fn stream_recv(&mut self, stream_id: StreamId, out: &mut [u8]) -> StreamResult<(usize, bool)>;

    /// Send a chunk of bytes,
    /// and optionally set `FIN` frame, which means a successful end of the stream direction.
    ///
    /// # Result
    /// If the result is `None`, then every single byte is saved
    /// and ready to be sent over a socket.
    /// Also, it means that the `FIN` flag was set too (if it was `true`).
    ///
    /// But, if the result is `Some`, it will contain the [Bytes] that were not saved,
    /// because the peer is not ready to receive more right now due to flow control window.
    /// Therefore, `FIN` flag was not applied too, and needs to be provided again.
    ///
    /// # Notes
    /// It is possible to provide empty [Bytes]:
    /// - Then `FIN=true` would mean "just send FIN frame".
    /// - And `FIN=false` is noop.
    ///
    /// [StreamError::Finish] is only returned on further attempts to send data (or `FIN`)
    /// when stream direction is already finished.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to write into server-unidirectional (or read only) stream.
    fn stream_send(
        &mut self,
        stream_id: StreamId,
        bytes: Bytes,
        fin: bool,
    ) -> StreamResult<Option<Bytes>>;

    /// Sets stream priority, if supported.
    ///
    /// More info: [Priority].
    fn stream_priority(&mut self, stream_id: StreamId, priority: Priority) -> StreamResult<()>;

    /// Returns a `streamID` that has available data.
    ///
    /// The same `streamID` won't be returned twice,
    /// unless all the data was consumed **&&** a new data arrived.
    fn stream_readable_next(&mut self) -> Option<StreamId>;

    /// Returns a `streamID` that is ready for new data.
    ///
    /// The same `streamID` won't be returned twice,
    /// unless it become available for new data again.
    fn stream_writable_next(&mut self) -> Option<StreamId>;

    /// Send a `STOP_SENDING(with custom u64 code)` frame,
    /// that tells the peer that we want to abort the stream direction
    /// and no longer interested in his data.
    ///
    /// If the direction is already closed, this operation is noop.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to send `STOP_SENDING` on client-unidirectional (or write only) stream.
    fn stream_stop_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()>;

    /// Send a `RESET_STREAM(with custom u64 code)` frame,
    /// that tells that we abort the stream direction and won't send the data again.
    ///
    /// If the direction is already closed, this operation is noop.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to send `RESET_STREAM` on server-unidirectional (or read only) stream.
    fn stream_reset_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()>;
}


/// A QUIC stream task that can be scheduled for the [StreamBackend].
///
/// Each entry corresponds to the [StreamBackend] method.
pub(crate) enum StreamTask {
    /// [StreamBackend::stream_recv], but with ownership of the buffer.
    ///
    /// It is guaranteed to write no more than [BytesMut::len].
    Recv(BytesMut),

    /// [StreamBackend::stream_send].
    Send(Bytes, bool),

    /// [StreamBackend::stream_priority].
    Priority(Priority),

    /// [StreamBackend::stream_stop_sending].
    StopSending(u64),

    /// [StreamBackend::stream_reset_sending].
    ResetSending(u64),
}

/// [StreamTask] result.
pub(crate) enum StreamTaskResult {
    /// [StreamBackend::stream_recv].
    Recv(StreamResult<(BytesMut, usize, bool)>),

    /// [StreamBackend::stream_send].
    Send(StreamResult<Option<Bytes>>),
}

impl StreamTask {
    /// Run the task with the [StreamBackend].
    pub fn run<B: StreamBackend>(
        self,
        stream_id: StreamId,
        backend: &mut B,
    ) -> Option<StreamTaskResult> {
        match self {
            Self::Recv(mut out) => {
                let result = backend
                    .stream_recv(stream_id, out.as_mut())
                    .map(|(read, fin)| (out, read, fin));

                Some(StreamTaskResult::Recv(result))
            }
            Self::Send(bytes, fin) => {
                let result = backend.stream_send(stream_id, bytes, fin);
                Some(StreamTaskResult::Send(result))
            }
            Self::Priority(priority) => {
                if let Err(e) = backend.stream_priority(stream_id, priority) {
                    debug!("unable to set stream priority to {priority:?}: {e}");
                }

                None
            }
            Self::StopSending(e) => {
                if let Err(e) = backend.stream_stop_sending(stream_id, e) {
                    debug!("unable to send 'STOP_SENDING' to peer: {e}");
                }

                None
            }
            Self::ResetSending(e) => {
                if let Err(e) = backend.stream_reset_sending(stream_id, e) {
                    debug!("unable to send 'RESET_SENDING' to peer: {e}");
                }

                None
            }
        }
    }
}

impl StreamTaskResult {
    pub fn name(&self) -> &str {
        match self {
            Self::Recv(_) => "Recv",
            Self::Send(_) => "Send",
        }
    }
}
