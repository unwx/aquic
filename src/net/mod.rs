use crate::exec::SendOnMt;
use std::future::Future;
use std::io::Result;
use std::net::{SocketAddr, ToSocketAddrs};

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

    /// Setup socket to ensure it will work correctly on current environment.
    ///
    /// For example, enable `SO_REUSEPORT` on `monoio` async runtime.
    fn setup(&self) -> Result<()>;
}

/// A reusable container for a single UDP packet.
///
/// This struct holds the packet buffer, the remote address, and the length
/// of the valid payload. It is designed to be reused to minimize allocations.
pub struct Msg {
    /// Buffer to send and receive data.
    pub(crate) buf: Vec<u8>,

    /// Never-empty address for sending, and always empty address for receiving (to be filled later).
    pub(crate) from: Option<SocketAddr>,

    /// Never-empty address for sending, and always empty address for receiving (to be filled later).
    pub(crate) to: Option<SocketAddr>,

    /// Number of bytes sent, or received.
    pub(crate) len: usize,
}

impl Msg {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0; capacity],
            from: None,
            to: None,
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

    /// Returns the 'from' socket address.
    ///
    /// # Panics
    ///
    /// Panics if the address has not been set (e.g., if the message was
    /// just created but not yet received).
    pub fn from(&self) -> SocketAddr {
        self.from.expect("socket address 'from' is not ready yet")
    }

    /// Sets the 'from' socket address.
    ///
    /// Use this when preparing a message to be sent.
    pub fn set_from(&mut self, from: SocketAddr) {
        self.from = Some(from);
    }

    /// Returns the 'to' socket address.
    ///
    /// # Panics
    ///
    /// Panics if the address has not been set (e.g., if the message was
    /// just created but not yet received).
    pub fn to(&self) -> SocketAddr {
        self.to.expect("socket address 'to' is not ready yet")
    }

    /// Sets the 'to' socket address.
    ///
    /// Use this when preparing a message to be sent.
    pub fn set_to(&mut self, to: SocketAddr) {
        self.to = Some(to);
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
        self.from = None;
        self.to = None;
        self.len = 0;
    }
}
