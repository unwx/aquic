use crate::conditional;
use crate::net::{Buf, BufMut, MultiMsgFlattenIter, SendMsg, ServerName, SoFeat};
use crate::runtime::AsyncRuntime;
use crate::stream::{Priority, StreamId};
use aquic_macros::doc_support;
use bytes::Bytes;
use std::collections::HashSet;
use std::fmt::{Debug, Display};
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;


conditional! {
    feature = "quiche",

    pub mod quiche;
}

conditional! {
    feature = "quinn",

    pub mod quinn;
}

mod error;
pub use error::*;

mod event;
pub use event::*;

mod firewall;
pub use firewall::*;

mod cid;
pub use cid::*;

mod token;
pub use token::*;


/// A QUIC connection ID may change from time to time for a single connection:
/// stable connection ID will never change.
///
/// A connection ID `Display` implementation provides a high-entropy string associated with the connection.
/// - It should **not** depend on a [QuicBackend] instance: (for example, do not do u64-like sequences).
/// - It should **not** repeat the same values after restart: (again, don't do u64-like sequences).
/// - It **should be** absolutely random, like UUIDv4, or a random hex string.
pub trait StableConnectionId: Debug + Display + Clone + Send + Unpin + 'static {}

/// QUIC protocol implementation provider.
///
/// This is a state machine: no network I/O is performed internally.
///
/// `QuicBackend` is intended to be invoked from the real network I/O loop,
/// and its state may only change when the I/O loop interacts with it.
///
/// # Logs, Tracing
///
/// Most of the logging is produced by I/O loop, which may log received events like [ConnectionEvent::New],
/// but `QuicBackend` may still produce its own logs if decides it's neccessary.
///
/// Please keep logs concise,
/// and keep in mind that there may be multiple instances of `QuicBackend` present,
/// for example one per CPU core.
///
/// Each log should have a [stable connection ID](StableConnectionId) (and stream ID if it's related) present.
#[doc_support]
pub trait QuicBackend: Sized {
    /// Implementation configuration.
    type Config;

    /// As QUIC connection IDs may change from time to time for a single connection,
    /// `StableConnectionId` will always stay the same.
    type StableConnectionId: StableConnectionId;

    /// Internal output buffer type.
    type OutBuf: Buf;


    /// Creates a new instance of `QuicBackend`.
    ///
    /// * `config`: configuration to use.
    /// * `listen_addr`: address where the local socket is bound to, **may** be unspecified.
    /// * `socket_features`: set of features that the I/O loop socket supports (per msg) and enabled (before binding).
    fn new(config: Self::Config, listen_addr: IpAddr, socket_features: &HashSet<SoFeat>) -> Self;

    /// Provides a hint, that current clocks can be updated with current time.
    ///
    /// **Note**: this is done before/after every I/O loop iteration or time-sensitive operation,
    /// therefore this method can be called with any delay: there is no interval.
    fn clock_update(&mut self);

    /// Returns container with previously written backend events inside.
    ///
    /// It is allowed to produce duplicate events,
    /// but discouraged if this leads to negative performance impact.
    ///
    /// It's recommended for the following events to be unique
    /// (that is, don't produce event with the same information twice):
    /// - [ConnectionEvent::New].
    /// - [ConnectionEvent::Active].
    /// - [ConnectionEvent::Closed],
    ///   [ConnectionEvent::Halted].
    /// - [StreamEvent::Reset].
    fn events(&mut self) -> &mut BackendEvents<Self::StableConnectionId>;


    /// Notifies that backend should prepare to shutdown.
    ///
    /// **Note**: all connections and streams are notified about incoming shutdown automatically, by the caller.
    fn close_prepare(&mut self, deadline: Instant);

    /// Polled after [`prepare_to_shutdown()`][QuicBackend::prepare_to_shutdown] to check,
    /// whether backend is ready to be closed or not (in case it's ready before the deadline).
    fn close_ready(&self) -> bool;

    /// Closes the `QuicBackend`,
    /// and then queues `CONNECTION_CLOSE` frames for all its connections with the specified application `err` and `reason`.
    /// This should be done as fast as possible.
    ///
    /// There is no need in writing backend events anymore.
    ///
    /// If backend is already closed, this operation is no-op.
    ///
    /// **Note**: if urgent, [`prepare_to_shutdown`](QuicBackend::prepare_to_shutdown) will not be called.
    fn close(&mut self, err: u64, reason: Bytes);


    /// Queues an attempt to connect to the specified peer,
    /// and returns a stable connection ID on success.
    ///
    /// Returns a stable connection ID on success.
    ///
    /// A [ConnectionEvent::New] must be generated on success.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::Illegal] backend doesn't serve as a client.
    /// - [Error::Other] other, uncovered error.
    fn connect(
        &mut self,
        server_name: &ServerName,
        peer_addr: SocketAddr,
    ) -> Result<Self::StableConnectionId>;

    /// Queues a `CONNECTION_CLOSE` frame with the specified application `err` and `reason`.
    ///
    /// Backend will **not** wait until all connection streams are finished/closed.
    ///
    /// A [ConnectionEvent::Closed] must be generated on success.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::Other] other, uncovered error.
    fn connection_close(
        &mut self,
        connection_id: &Self::StableConnectionId,
        err: u64,
        reason: Bytes,
    ) -> Result<()>;


    /// Writes packets to be sent to peers into `&mut out`.
    /// May write nothing, if there is no packet or buffer available.
    ///
    /// Additionally, handles internal events after the [`sleep()`][QuicBackend::sleep].
    ///
    /// **It is guaranteed**, that buffers are dropped in the same order
    /// they were inserted into `&mut out`.
    fn send(&mut self, out: &mut Vec<SendMsg<Self::OutBuf>>);

    /// Receives packets from peers.
    ///
    /// Generates outgoing [ConnectionEvent], [StreamEvent], [DatagramEvent] events;
    /// writes immediate outgoing packets into `&mut out_messages`.
    ///
    /// **Notes**:
    /// - After this method the [`send_prepare()`](QuicBackend::send_prepare) is going to be invoked,
    ///   therefore, it's not recommended to write non-immediate data like stream/datagram data, etc, to `&mut out_messages`;
    ///   a good practice would be to write `initial`/`version-negotiation`/`retry` packets only.
    /// - At least one message must be handled after all internal buffers are returned via [`send_done()`][QuicBackend::send_done],
    ///   or it will be considered as fatal error.
    fn recv<B: BufMut>(
        &mut self,
        in_packets: &mut MultiMsgFlattenIter<B>,
        out_messages: &mut Vec<SendMsg<Self::OutBuf>>,
    );


    /// Sleeps until there is an internal event that must be handled,
    /// for example connection timeout.
    ///
    /// On wake-up, [`send_prepare()`][QuicBackend::send_prepare] will be invoked.
    ///
    /// It's recommended to take a look at [`WheelTimer`][`crate::util::WheelTimer`],
    /// as it might be useful.
    ///
    /// # Cancel Safety
    ///
    /// This method is cancel safe, with no side-effects on future drop.
    fn sleep<AR: AsyncRuntime>(&mut self) -> impl Future<Output = ()>;


    /// Notifies that **client** application switched the network.
    ///
    /// Optionally provides a new address the socket listens to.
    /// Must be specified.
    ///
    /// At the moment, servers cannot initiate active network migration:
    /// [RFC 9000, Section 9](https://datatracker.ietf.org/doc/html/rfc9000#section-9-6).
    ///
    /// If the server is behind a Load-Balancer though
    /// and it desires to migrate to a new machine (using [CRIU](https://github.com/checkpoint-restore/criu) for example),
    /// [`migrate_scids()`](QuicBackend::migrate_scids) may be useful.
    ///
    /// Active migration will be performed on every client (locally-initiated) connection.
    ///
    /// If connection is unable to perform active migration,
    /// for example:
    /// - due to lack of spare destination IDs,
    /// - because of using zero-len source IDs,
    /// - a connection has not completed a handshake,
    ///
    /// a [ConnectionEvent::MigrationFailed] event must be generated.
    ///
    /// **It is forbidden** to perform an active migration using old
    /// connection IDs: [RFC 9000, Section 9](https://datatracker.ietf.org/doc/html/rfc9000#section-9.5-3).
    ///
    /// **It is forbidden** to send any frame/packet on unmigrated connections,
    /// including `CONNECTION_CLOSE`, due to the privacy leak.
    ///
    /// # Error
    ///
    /// - [Error::Closed]  backend is closed.
    /// - [Error::Illegal] `new_listen_addr` is `Some(unspecified)`.
    fn migrate_network(&mut self, new_listen_addr: Option<IpAddr>) -> Result<()>;

    /// Returns the peer's certificate chain (if any) as a vector of DER-encoded
    /// buffers.
    ///
    /// The certificate at index 0 is the peer's leaf certificate, the other
    /// certificates (if any) are the chain certificate authorities used to
    /// sign the leaf certificate.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] there is no connection associated with the provided ID.
    /// - [Error::Other] other, uncovered error.
    fn peer_cert_chain(&mut self, connection_id: &Self::StableConnectionId)
    -> Result<Vec<Vec<u8>>>;


    /// Writes an ordered stream data into `&mut out`,
    /// returning the number of bytes received, and whether it's a successful end of the stream or not (`FIN` flag).
    ///
    /// If there is no data to available (returns `(0, false)`),
    /// an application should wait for an [StreamEvent::Available] event before calling `stream_recv()` again.
    ///
    /// Backend implementation **may** forget the stream after notifying the application about its end.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::Illegal] the provided `stream_id` is a client unidirectional one.
    /// - [Error::BufferSize] the provided `out` buffer is empty.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::UnknownStream] the stream doesn't exist, or it's finished, or it was reset/stopped.
    /// - [Error::StreamReset] the stream is terminated by the peer;
    ///   **may** be returned only once, with further calls returning `UnknownStream` instead.
    /// - [Error::Other] other, uncovered error.
    fn stream_recv(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        out: &mut [u8],
    ) -> Result<(usize, bool)>;

    /// Tries to open a stream of a certain directionality,
    /// and performs [`stream_send()`][QuicBackend::stream_send] on success.
    ///
    /// Generates an [StreamEvent::Available] event
    /// and returns a new opened stream ID on success.
    ///
    /// If the streams in the given direction are currently exhausted,
    /// a [Error::StreamsExhausted] will be returned
    /// and an application should wait for a next [ConnectionEvent::StreamsAvailable] event.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::Illegal] connection has not progressed enough to open streams.
    /// - [Error::StreamsExhausted] the streams in the given direction are currently exhausted.
    /// - [Error::Other] other, uncovered error.
    fn stream_open_send(
        &mut self,
        connection_id: &Self::StableConnectionId,
        bidirectional: bool,
        bytes: Bytes,
        fin: bool,
    ) -> Result<(StreamId, Option<Bytes>)>;

    /// Sends `bytes` with an optional `FIN` flag (if `true`),
    /// which means a successful end of the stream direction.
    ///
    /// Returns `None` on complete send operation,
    /// or `Some(unsent_bytes)` on a partial write.
    ///
    /// If a partial write happens,
    /// `FIN` flag is ignored, and
    /// an application should wait for an [StreamEvent::Available] event before calling `stream_send()` again.
    ///
    /// It is possible to provide empty `bytes` with `FIN` flag set to true.
    /// Partial write is impossible in this case.
    ///
    /// Backend implementation **may** forget the stream after sending a `FIN`,
    /// or after notifying the application about its end.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::Illegal] the provided `stream_id` is a server unidirectional one.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::UnknownStream] the stream doesn't exist, or it's finished, or it was reset/stopped.
    /// - [Error::StreamStop] the specified stream is terminated by the peer;
    ///   **may** be returned only once, with further calls returning `UnknownStream` instead.
    /// - [Error::Other] other, uncovered error.
    fn stream_send(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        bytes: Bytes,
        fin: bool,
    ) -> Result<Option<Bytes>>;

    /// Sends a `STOP_SENDING(err)` frame
    /// that tells the peer that application is no longer interested in peer's data.
    ///
    /// If the backend, connection or direction is already closed,
    /// this operation is no-op.
    ///
    /// Does nothing on client-unidirectional (or write only) streams as well.
    fn stream_stop_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    );

    /// Sends a `RESET_STREAM(err)` frame
    /// that tells the peer that application wants to abort the stream direction and won't send more data.
    ///
    /// If the backend, connection or direction is already closed,
    /// this operation is no-op.
    ///
    /// Does nothing on server-unidirectional (or read only) streams as well.
    fn stream_reset_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    );

    /// Sets a priority for the specified stream, if supports.
    ///
    /// If the backend, connection or direction is closed,
    /// this operation is no-op.
    ///
    /// If the backend doesn't support this feature, it's also just no-op.
    #[doc_support(quinn, quiche)]
    fn stream_set_priority(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        priority: Priority,
    );


    /// Writes a single datagram into `&mut out`,
    /// returning the size of the datagram.
    ///
    /// If there is no datagram to available (returns `0`),
    /// an application should wait for an [DatagramEvent::InAvailable] event before calling `dgram_recv()` (or its alternative) again.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::DgramDisabled] datagram support is disabled.
    /// - [Error::BufferSize] the provided `out` buffer cannot fit the datagram;
    ///   the datagram might be dropped afterwards.
    /// - [Error::Other] other, uncovered error.
    fn dgram_recv(
        &mut self,
        connection_id: &Self::StableConnectionId,
        out: &mut [u8],
    ) -> Result<usize>;

    /// Alternative, zero-copy version of the [`dgram_recv()`](QuicBackend::dgram_recv).
    ///
    /// It's supposed to be faster, or at least to be as fast as `dgram_recv()` on most backends,
    /// as it provides a reference to the underlying datagram without copying it.
    ///
    /// If there is no datagram to available (returns `None`),
    /// an application should wait for an [DatagramEvent::InAvailable] event before calling `dgram_recv()` (or its alternative) again.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::DgramDisabled] datagram support is disabled.
    /// - [Error::Other] other, uncovered error.
    fn dgram_recv_zc<F, R>(
        &mut self,
        connection_id: &Self::StableConnectionId,
        consumer: F,
    ) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> R;

    /// Sends a datagram.
    ///
    /// Returns `None` on success, or `Some(original_datagram)` if backend's internal buffers are full.
    /// In this case, an application should wait for an [DatagramEvent::OutAvailable] event before calling `dgram_send()` again.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] the connection doesn't exist, or it's draining, or it's closed.
    /// - [Error::DgramDisabled] datagram support is disabled by config, provider, or peer.
    /// - [Error::BufferSize] datagram exceeds [`dgram_send_max_size`](QuicBackend::dgram_send_max_size).
    /// - [Error::Other] other, uncovered error.
    fn dgram_send(
        &mut self,
        connection_id: &Self::StableConnectionId,
        bytes: Bytes,
    ) -> Result<Option<Bytes>>;

    /// Returns a maximum size of a single outgoing datagram: `min(pmtu, peer_max_dgram_len)`,
    /// or `zero` if it's disabled.
    ///
    /// # Error
    ///
    /// - [Error::Closed] backend is closed.
    /// - [Error::UnknownConnection] there is no connection associated with the provided `connection_id`.
    /// - [Error::Other] other, uncovered error.
    fn dgram_send_max_size(&mut self, connection_id: &Self::StableConnectionId) -> Result<usize>;
}
