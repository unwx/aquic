use crate::exec::SendOnMt;
use std::future::Future;
use std::io::Result;
use std::net::SocketAddr;


mod types;
pub use types::*;


/// A socket to be used for QUIC connections.
///
/// In theory application may provide a socket implementation that works not on UDP, but on something else.
/// Though I believe it may not worth it...
///
/// In ideal scenario, QUIC backend and Socket will support GSO/GRO and sendmmsg/recvmmsg combined.
/// Therefore we might send many different packets for different connections in a single syscall, like:
/// - 1 GSO packet for CID `A`,
/// - 3 packets that are not padded to the previous packet GSO size for CID `A`,
/// - 2 packets for CID `B`.
///
/// Ideally, the total payload of these packets should not exceed the socket's buffer.
pub trait Socket: Sized {
    /// Creates and binds a new socket to the specified address.
    ///
    /// Implementation **shoud** attempt to:
    /// - Reuse port (for example, `SO_REUSEPORT` on Linux) to permits multiple sockets to be bound to an identical socket address.
    /// - Enable GRO (for example, `UDP_GRO` on Linux) to make the socket receive multiple datagrams worth of data as a single large buffer.
    /// - Set Don't Fragment (for example, `IP_DONTFRAG` with `IP_PMTUDISC_DO` on Linux) to make socket return an error if packet size exceeds MTU.
    fn bind(addr: SocketAddr) -> impl Future<Output = Result<Self>> + SendOnMt;

    /// Returns an address this socket is bound to.
    ///
    /// **May** be unspecified.
    fn source_addr(&self) -> SocketAddr;

    /// Sends a batch of messages.
    ///
    /// Returns the number of messages successfully flushed to the network.
    ///
    /// In case of partial write, the caller should wait until [`ready_to_send`](Socket::ready_to_send) returns.
    fn send<'a, B: Buf<'a>>(
        &self,
        messages: &[SendMsg<B>],
    ) -> impl Future<Output = Result<usize>> + SendOnMt;

    // Waits until the socket become ready to send packets again.
    fn ready_to_send(&self) -> impl Future<Output = ()> + SendOnMt;

    /// Receives a batch of messages.
    ///
    /// The `msgs` slice will be filled with incoming packets.
    /// Returns the number of messages successfully read.
    fn recv<'a, B: BufMut<'a>>(
        &self,
        msgs: &mut [RecvMsg<B>],
    ) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Returns features that socket has enabled (on `bind`) and supports (per message).
    fn features(&self) -> impl Iterator<Item = SoFeat> + '_;
}
