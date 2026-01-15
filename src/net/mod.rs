use crate::exec::SendOnMt;
use std::future::Future;
use std::io::Result;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

/// A unified async interface for sockets.
pub trait Socket: Sized {
    /// Creates and binds a new socket to the specified address.
    fn bind<A: ToSocketAddrs>(addr: A) -> impl Future<Output = Result<Self>> + SendOnMt;

    /// Receives a batch of messages.
    ///
    /// The `messages` slice will be filled with incoming packets.
    /// Returns the number of messages successfully read.
    fn recv(&self, messages: &mut [Msg]) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Sends a batch of messages.
    ///
    /// Returns the number of messages successfully flushed to the network.
    fn send(&self, messages: &[Msg]) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Enable port reuse for this socket.
    ///
    /// (`SO_REUSEPORT` on Linux).
    fn enable_port_reuse(&self) -> Result<()>;
}

/// A reusable container for a single UDP packet.
///
/// This struct holds the packet buffer, the remote address, and the length
/// of the valid payload. It is designed to be reused to minimize allocations.
pub struct Msg {
    /// Buffer to send and receive data.
    pub(crate) buf: Vec<u8>,

    /// Never-empty address for sending, and always empty address for receiving (to be filled later).
    pub(crate) addr: Option<SocketAddr>,

    /// Number of bytes sent, or received.
    pub(crate) len: usize,
}

impl Msg {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0; capacity],
            addr: None,
            len: 0,
        }
    }

    /// Returns a slice of the underlying buffer.
    ///
    /// Note: This returns the *entire* allocated buffer. Use [`len`](Self::len)
    /// to determine how much of this buffer contains valid data.
    pub fn buf(&self) -> &[u8] {
        self.buf.as_slice()
    }

    /// Returns a mutable slice of the underlying buffer.
    pub fn buf_mut(&mut self) -> &mut [u8] {
        self.buf.as_mut_slice()
    }

    /// Returns the remote socket address.
    ///
    /// # Panics
    ///
    /// Panics if the address has not been set (e.g., if the message was
    /// just created but not yet received).
    pub fn addr(&self) -> SocketAddr {
        self.addr.expect("socket address is not ready yet")
    }

    /// Sets the remote socket address.
    ///
    /// Use this when preparing a message to be sent.
    pub fn set_addr(&mut self, addr: SocketAddr) {
        self.addr = Some(addr);
    }

    /// Returns the length of the valid payload in the buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Sets the length of the valid payload.
    ///
    /// This is typically updated by the kernel after a receive operation.
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    /// Resize the internal buffer.
    pub(crate) fn resize(&mut self, capacity: usize) {
        self.buf.resize(capacity, 0);
    }

    /// Resets the message state for reuse.
    pub(crate) fn reset(&mut self) {
        self.addr = None;
        self.len = 0;
    }
}
