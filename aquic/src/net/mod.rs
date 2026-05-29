use crate::SendOnMt;
use std::future::Future;
use std::io::Result;
use std::net::SocketAddr;


mod buf;
pub use buf::*;

mod types;
pub use types::*;


/// A socket to be used for QUIC connections.
pub trait Socket: Sized {
    /// Creates and binds a new socket to the specified address.
    ///
    /// Implementation **shoud** attempt to:
    /// - Reuse port (for example, `SO_REUSEPORT` on Linux) to permit multiple sockets to be bound to an identical socket address.
    /// - Enable GRO (for example, `UDP_GRO` on Linux) to make the socket receive multiple datagrams worth of data as a single large buffer.
    /// - Set Don't Fragment (for example, `IP_DONTFRAG` with `IP_PMTUDISC_DO` on Linux) to make socket return an error if packet size exceeds MTU.
    fn bind(addr: SocketAddr) -> impl Future<Output = Result<Self>> + SendOnMt;

    /// Returns an address this socket is bound to.
    ///
    /// IP **may** be unspecified, but port must always be specified.
    fn listen_addr(&self) -> SocketAddr;

    /// Sends a batch of messages.
    ///
    /// Returns the number of messages successfully flushed to the network.
    ///
    /// In case of partial write, the caller should wait until [`send_unblocked`](Socket::send_unblocked) returns.
    fn send<B: Buf>(&self, msgs: &[SendMsg<B>]) -> Result<usize>;

    /// Waits until the socket become ready to send packets again.
    fn send_unblocked(&self) -> impl Future<Output = ()>;

    /// Receives a batch of messages.
    ///
    /// The `&mut msgs` slice will be filled with incoming packets.
    /// Returns the number of messages successfully read.
    fn recv<B: BufMut>(
        &self,
        msgs: &mut [RecvMsg<B>],
    ) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Returns features that socket has enabled (on `bind`) and supports (per message).
    fn features(&self) -> impl Iterator<Item = SoFeat> + '_;
}
