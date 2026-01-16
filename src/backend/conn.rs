use crate::backend::StreamBackend;
use std::borrow::Cow;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use std::time::Instant;

/// Generic QUIC Connection error.
///
/// Receiving it instantly means that [ConnBackend] has closed the connection.
#[derive(Debug, Clone)]
pub struct ConnError(pub Cow<'static, str>);

impl Display for ConnError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "connection error ({})", &self.0)
    }
}

impl Error for ConnError {}


/// QUIC Connection operation result.
pub type ConnResult<T> = Result<T, ConnError>;


/// A QUIC Connection API for various implementations of the QUIC protocol.
///
/// **Note**: protocol does not work with sockets.
/// It has its own state, buffers.
/// Data is received/sent from/over the socket in the networking part of this library.
pub trait ConnBackend: StreamBackend + Sized {
    // TODO(feat) dgrams, events.
    type Config;


    /// Creates a new server-side connection.
    ///
    /// The `source_id` parameter represents the server's source connection ID, while
    /// the optional `original_source_id` parameter represents the original destination ID the
    /// client sent before a stateless retry
    /// (this is only required when using the [`retry()`](Self::retry) function).
    fn accept(
        source_id: &[u8],
        original_source_id: Option<&[u8]>,
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
        config: &mut Self::Config,
    ) -> ConnResult<Self>;

    /// Creates a new client-side connection.
    ///
    /// The `source_id` parameter is used as the connection's source connection ID,
    /// while the optional `server_name` parameter is used to verify the peer's
    /// certificate.
    fn connect(
        server_name: Option<&str>,
        source_id: &[u8],
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
        config: &mut Self::Config,
    ) -> ConnResult<Self>;

    /// Writes a stateless retry packet.
    ///
    /// The `source_id` and `destination_id` parameters are the source connection ID and the
    /// destination connection ID extracted from the received client's Initial
    /// packet, while `new_source_id` is the server's new source connection ID and
    /// `token` is the address validation token the client needs to echo back.
    ///
    /// The application is responsible for generating the address validation
    /// token to be sent to the client, and verifying tokens sent back by the
    /// client. The generated token should include the `destination_id` parameter, such
    /// that it can be later extracted from the token and passed to the
    /// [`accept()`](Self::accept) function as its `original_source_id` parameter.
    fn retry(
        source_id: &[u8],
        destination_id: &[u8],
        new_source_id: &[u8],
        token: &[u8],
        version: u32,
        out: &mut [u8],
    ) -> ConnResult<usize>;

    /// Writes a version negotiation packet.
    ///
    /// The `scid` and `dcid` parameters are the source connection ID and the
    /// destination connection ID extracted from the received client's Initial
    /// packet that advertises an unsupported version.
    fn negotiate_version(
        source_id: &[u8],
        destination_id: &[u8],
        out: &mut [u8],
    ) -> ConnResult<usize>;

    /// Returns true if the given protocol version is supported.
    fn is_version_supported(version: u32) -> bool;


    /// Writes a single QUIC packet to be sent to the peer.
    ///
    /// On success the number of bytes written to the output buffer is
    /// returned, or `None` if there was nothing to write.
    ///
    /// This method should be called multiple times until `None` is
    /// returned, indicating that there are no more packets to send.
    ///
    /// It is recommended that `send()` be called in the following cases:
    ///
    ///  * When the application receives QUIC packets from the peer (that is,
    ///    any time [`recv()`](Self::recv) is also called).
    ///
    ///  * When the connection timer expires (that is, any time [`on_timeout()`](Self::on_timeout)
    ///    is also called).
    ///
    ///  * When the application sends data to the peer (for example, any time
    ///    [`stream_send()`](Self::stream_send) or `stop/reset sending` are called).
    ///
    ///  * When the application receives data from the peer (for example any
    ///    time [`stream_recv()`](Self::stream_recv) is called).
    ///
    /// Once [`is_draining()`](Self::is_draining) returns `true`, it is no longer necessary to call
    /// `send()` and all calls will return `None`.
    fn send(&mut self, out: &mut [u8]) -> ConnResult<Option<(usize, Address)>>;

    /// Processes QUIC packets received from the peer.
    ///
    /// Coalesced packets will be processed as necessary.
    ///
    /// Note that the contents of the input buffer `buf` might be modified by
    /// this function due to, for example, in-place decryption.
    fn recv(&mut self, buf: &mut [u8], addr: Address) -> ConnResult<()>;


    /// Returns `true` if connection is closed.
    fn is_closed(&self) -> bool;

    /// Returns `true` if the connection is draining.
    ///
    /// If this returns `true`, the connection object cannot yet be dropped, but
    /// no new application data can be sent or received. An application should
    /// continue calling the [`recv()`](Self::recv), [`timeout()`](Self::next_timeout),
    /// and [`on_timeout()`](Self::on_timeout) methods as normal,
    /// until the [`is_closed()`](Self::is_closed) method returns `true`.
    ///
    /// In contrast, once `is_draining()` returns `true`, calling [`send()`](Self::send)
    /// is not required because no new outgoing packets will be generated.
    fn is_draining(&self) -> bool;

    /// Returns `true` if the connection handshake is complete.
    fn is_established(&self) -> bool;

    /// Returns true if the connection has a pending handshake that has
    /// progressed enough to send or receive early data.
    fn is_in_early_data(&self) -> bool;


    /// Returns pacing decision regarding the next packet.
    fn next_pacer_decision(&self, now: Instant) -> PacerDecision;

    /// Returns an [Instant] when to call [`on_timeout`](Self::on_timeout),
    /// or `None` if `on_timeout` should not be invoked.
    fn next_timeout(&self, now: Instant) -> Option<Instant>;

    /// Processes a timeout event.
    fn on_timeout(&mut self);


    /// Returns the peer's leaf certificate (if any) as a DER-encoded buffer.
    fn peer_cert(&self) -> Option<&[u8]>;

    /// Returns the peer's certificate chain (if any) as a vector of DER-encoded
    /// buffers.
    ///
    /// The certificate at index 0 is the peer's leaf certificate, the other
    /// certificates (if any) are the chain certificate authorities used to
    /// sign the leaf certificate.
    fn peer_cert_chain(&self) -> Option<Vec<&[u8]>>;
}


/// Socket source and destination address.
#[derive(Debug, Copy, Clone)]
pub struct Address {
    /// Source address.
    pub source: SocketAddr,

    /// Destination address.
    pub destination: SocketAddr,
}

/// Pacing algorithm decision regarding a next packet:
/// when it should be sent and if it can be part of a burst.
#[derive(Debug, Copy, Clone)]
pub enum PacerDecision {
    At(Instant),
    Burst,
}
