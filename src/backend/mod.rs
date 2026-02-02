use crate::backend::cid::ConnectionIdGenerator;
use crate::conditional;
use crate::core::ConstBuf;
use crate::exec::SendOnMt;
use crate::net::{Buf, MAX_PACKET_SIZE, RecvMsg, SendMsg, ServerName, SoFeat};
use crate::stream::{Chunk, Priority, StreamId};
use aquic_macros::supports;
use bytes::Bytes;
use smallvec::SmallVec;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Instant;

conditional! {
    feature = "quiche",

    mod quiche;
    pub use quiche::*;
}

conditional! {
    feature = "quinn",

    mod quinn;
    pub use quinn::*;
}

pub mod cid;
pub mod stream;
pub mod util;


mod types;
pub use types::*;


// By design,
// a single `QuicBackend` should be able serve multiple [`protocol specifications`](crate::Spec).

/// QUIC protocol implementation provider.
///
/// This is a state machine, therefore no network I/O is performed internally:
/// `QuicBackend` is intended to be invoked from real network I/O loop.
#[supports]
pub trait QuicBackend: Sized {
    // TODO(feat): dgrams.

    /// Implementation configuration.
    type Config;

    /// Scheduled event, to wake up [QuicBackend] later with.
    type WakeEvent;

    /// As QUIC connection IDs may change from time to time for a single connection,
    /// `StableConnectionId` should always stay the same.
    type StableConnectionId: StableConnectionId;

    /// Generator for temporary QUIC connection IDs.
    type ConnectionIdGenerator: ConnectionIdGenerator;

    /// Internal output buffer type.
    type OutBuf: Buf;


    /// Creates a new instance of `QuicBackend`.
    ///
    /// * `config`: configuration to use.
    /// * `connection_id_generator`: connection ID generator.
    /// * `socket_addr`: address where the local socket is bound to.
    /// * `socket_features`: set of features that the I/O loop socket supports (per msg) and enabled (before binding).
    fn new(
        config: Self::Config,
        connection_id_generator: Self::ConnectionIdGenerator,
        socket_addr: SocketAddr,
        socket_features: &HashSet<SoFeat>,
    ) -> Self;

    /// Updates the internal clock with the current time.
    ///
    /// **Note**: this is done before every I/O loop iteration or time-sensitive operation like [`on_alarm()`][QuicBackend::on_alarm],
    /// therefore this method can be called with any delay: there is no interval.
    fn tick(&mut self, now: Instant);


    /// Notifies that backend should prepare to shutdown.
    fn prepare_to_shutdown(&self, deadline: Instant);

    /// Polled after [`prepare_to_shutdown()`][QuicBackend::prepare_to_shutdown] to check,
    /// whether backend is ready to be closed or not (in case it's ready before the deadline).
    ///
    /// **Note**: all connections and streams are notified about incoming shutdown automatically.
    fn ready_to_shutdown(&self) -> bool;

    /// Closes the `QuicBackend`,
    /// and then queues `CONNECTION_CLOSE` frames for all its connections with the specified application `err` and `reason`.
    /// This should be done as fast as possible.
    ///
    /// If backend is already closed, this operation is no-op.
    ///
    /// **Note**: if urgent, [`prepare_to_shutdown`](QuicBackend::prepare_to_shutdown) will not be called.
    fn close(&mut self, err: u64, reason: Bytes);


    /// Queues an attempt to connect to the specified peer.
    ///
    /// # Returns
    ///
    /// - Stable connection ID on success.
    /// - [Error::Illegal] if backend serves as server-only.
    /// - [Error::Closed] if backend is closed.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional.
    fn connect(
        &mut self,
        server_name: &ServerName,
        peer_addr: SocketAddr,
    ) -> Result<Self::StableConnectionId>;

    /// Queues a `CONNECTION_CLOSE` frame with the specified application `err` and `reason`.
    /// This method does **not** wait until all connection streams are finished/closed.
    ///
    /// If backend is closed, this operation is no-op.
    ///
    /// # Returns
    ///
    /// - [Error::UnknownConnection] if there is no such connection (**may** be closed earlier).
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    fn connection_close(
        &mut self,
        connection_id: &Self::StableConnectionId,
        err: u64,
        reason: Bytes,
    ) -> Result<()>;


    /// Writes packets to be sent to the peer into `messages`.
    /// If there are pending events available, they can be written into `events` too.
    ///
    /// **May** write nothing, if there is no packet or buffer available.
    ///
    /// # Returns
    ///
    /// This method will never return [Error::Closed], as after [`close()`][QuicBackend::close]
    /// there may be pending `CONNECTION_CLOSE` frames.
    ///
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    fn send_prepare(
        &mut self,
        messages: &mut Vec<SendMsg<Self::OutBuf>>,
        events: &mut OutEvents<Self::StableConnectionId>,
    ) -> Result<()>;

    /// Notifies that some or all of the previous packets were flushed,
    /// and you may reuse their buffers again.
    ///
    /// As it was mentioned before, partial writes might happen:
    /// therefore not every buffer may return in a single `send_done()` call.
    /// In such cases, there will be multiple `send_done()` calls.
    ///
    /// **It is guaranteed**, that buffers are returned in the same order
    /// they were provided in [`send_prepare()`][QuicBackend::send_prepare] as messages.
    fn send_done<I: IntoIterator<Item = Self::OutBuf>>(&mut self, buffers: I);

    /// Receives packets from peer,
    /// handles them,
    /// and writes outgoing events into `events`.
    ///
    /// **Note**: this method **must** handle at least one message after all internal buffers are returned via [`send_done()`][QuicBackend::send_done],
    /// or it will be considered as fatal error.
    ///
    /// # Returns
    ///
    /// This method will never return [Error::Closed], as after [`close()`][QuicBackend::close]
    /// I/O loop may wait a little for incoming packets to ensure all peers received all `CONNECTION_CLOSE` frames.
    ///
    /// - The number of messages that were handled.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    ///
    /// **Note**: If the returned number of messages is not equal to `messages.len()`,
    /// this method will be invoked again **only** after [`send_prepare()`][QuicBackend::send_prepare] & [`send_done()`][QuicBackend::send_done].
    fn recv(
        &mut self,
        messages: &mut [RecvMsg<ConstBuf<MAX_PACKET_SIZE>>],
        events: &mut OutEvents<Self::StableConnectionId>,
    ) -> Result<usize>;


    /// Sleep until there is an internal event that must be handled,
    /// for example connection timeout.
    ///
    /// It's recommended to take a look at [`TimerWheel::next()`][`crate::backend::util::TimerWheel::next`],
    /// as it might be useful.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe, with no side-effects on future drop.
    fn sleep(&mut self) -> impl Future<Output = ()> + SendOnMt;

    /// Invoked when [`sleep()`][QuicBackend::sleep] gets interrupted.
    ///
    /// If backend is closed, this operation is no-op.
    ///
    /// # Returns
    ///
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    fn on_alarm(&mut self) -> Result<()>;


    /// Returns the peer's certificate chain (if any) as a vector of DER-encoded
    /// buffers.
    ///
    /// The certificate at index 0 is the peer's leaf certificate, the other
    /// certificates (if any) are the chain certificate authorities used to
    /// sign the leaf certificate.
    ///
    /// Errors:
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no such connection (**may** be closed earlier).
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional.
    fn peer_cert_chain(&mut self, connection_id: &Self::StableConnectionId)
    -> Result<Vec<Vec<u8>>>;


    /// Checks, whether the application can open a new stream of a certain directionality.
    /// Preserves a stream slot on success.
    ///
    /// This is strictly a local operation, therefore [`send_prepare()`][QuicBackend::send_prepare] **may not** invoked after this method.
    ///
    /// # Returns
    ///
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no connection with provided `connection_id`.
    /// - [Error::StreamsExhausted] if the streams in the given direction are currently exhausted.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    fn stream_open(
        &mut self,
        connection_id: &Self::StableConnectionId,
        bidirectional: bool,
    ) -> Result<StreamId>;

    /// # Returns
    ///
    /// - Chunks of stream data, and whether the stream is finished (bool `FIN` flag).
    ///   The number of chunks **must not** exceed `32`, and the total number of bytes **should not** exceed `threshold`.
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no connection with provided `connection_id`.
    /// - [Error::UnknownStream] if there is no stream with provided `stream_id`, or stream was previously finished/terminated.
    /// - [Error::StreamFinish] if the specified stream is finished.
    /// - [Error::StreamResetSending] if the specified stream is terminated by peer.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    ///
    /// **Note**: implementation **may not** return [Error::StreamFinish], [Error::StreamResetSending] errors,
    /// if application was notified about these events earlier (e.g. `stream_recv()` returned `FIN`, or there was [StreamEvent::InReset]), and the stream was freed.
    ///
    /// # Panics
    ///
    /// Implementation **might** panic on attempt to read from client-unidirectional (or write only) stream.
    fn stream_recv(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        threshold: usize,
    ) -> Result<(SmallVec<[Chunk; 32]>, bool)>;

    /// Sends a chunk of bytes,
    /// and optionally sets `FIN` frame, which means a successful end of the stream direction.
    ///
    /// Returns `false` if partial write happened, and application need to invoke this method again.
    /// All sent values are removed from the `&mut chunks` vector.
    ///
    /// **Note**: it is possible to provide empty chunk with `FIN` flag set to true.
    ///
    /// # Returns
    ///
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no connection with provided `connection_id`.
    /// - [Error::UnknownStream] if there is no stream with provided `stream_id`: it might be closed or previously terminated.
    /// - [Error::StreamFinish] if the specified stream is finished.
    /// - [Error::StreamStopSending] if the specified stream is terminated by peer.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    ///
    /// **Note**: implementation **may not** return [Error::StreamFinish], [Error::StreamStopSending] errors,
    /// if application was notified about these events earlier (e.g. `stream_recv()` returned `FIN`, or there was [StreamEvent::OutReset]), and the stream was freed.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to write into server-unidirectional (or read only) stream.
    fn stream_send(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        chunks: &mut SmallVec<[Bytes; 32]>,
        fin: bool,
    ) -> Result<bool>;

    /// Sends a `STOP_SENDING(err)` frame,
    /// that tells to peer that we want to abort the stream direction
    /// and no longer interested in his data.
    ///
    /// If the backend, connection or direction is already closed,
    /// this operation is no-op.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to send `STOP_SENDING` on client-unidirectional (or write only) stream.
    fn stream_stop_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    );

    /// Send a `RESET_STREAM(err)` frame,
    /// that tells that we abort the stream direction and won't send more data.
    ///
    /// If the backend, connection or direction is already closed,
    /// this operation is no-op.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to send `RESET_STREAM` on server-unidirectional (or read only) stream.
    fn stream_reset_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    );

    /// Sets stream priority for the specified stream, if supported.
    ///
    /// # Returns
    ///
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no connection with provided `connection_id`.
    /// - [Error::UnknownStream] if there is no stream with provided `stream_id`.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to set priority on server-unidirectional (or read only) stream.
    /// Priorities may be set only locally, as only the local QUIC provider understands them.
    fn stream_set_priority(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        priority: Priority,
    ) -> Result<()>;

    /// Makes [`stream_recv()`][QuicBackend::stream_recv] return unordered chunks of data,
    /// if supported: [Chunk::Unordered].
    ///
    /// This is useful (less latency on network with high packet-loss rate),
    /// when application doesn't care about packets ordering for the specific stream.
    ///
    /// # Returns
    ///
    /// - [Error::Closed] if backend is closed.
    /// - [Error::UnknownConnection] if there is no connection with provided `connection_id`.
    /// - [Error::UnknownStream] if there is no stream with provided `stream_id`.
    /// - [Error::Other] on other, unexpected error.
    /// - [Error::Fatal] if backend is no longer functional due to internal error.
    ///
    /// # Panics
    ///
    /// Implementation might panic on attempt to set unordered on client-unidirectional (or write only) stream.
    #[supports(quinn)]
    fn stream_set_unordered(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
    ) -> Result<()>;
}
