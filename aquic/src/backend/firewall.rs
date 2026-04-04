use std::net::SocketAddr;

/// A simple QUIC backend level firewall.
///
/// It **must not** replace any existing firewall.
/// It exists only to make certain connections decisions
/// based on local observations.
pub trait Firewall {
    /// Returns `true` if a QUIC backend may accept a new connection,
    /// without verifying peer's real identity using `Retry` packet.
    ///
    /// The [anti-amplification limit](https://datatracker.ietf.org/doc/html/rfc9000#section-8-2)
    /// will be applied for this connection.
    ///
    /// This method is invoked only for `Initial` packets with an absent or invalid
    /// token.
    fn allow_unverified(
        &self,
        source_addr: SocketAddr,
        total_server_connections: usize,
        total_unverified_connections: usize,
    ) -> bool;
}

#[derive(Debug, Clone)]
pub struct StrictFirewall;

impl Firewall for StrictFirewall {
    fn allow_unverified(&self, _: SocketAddr, _: usize, _: usize) -> bool {
        false
    }
}
