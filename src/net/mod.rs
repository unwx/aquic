use crate::exec::SendOnMt;
use bytes::BytesMut;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::io::Result;
use std::net::SocketAddr;
use std::time::Instant;

/// A unified async interface for sockets.
pub trait Socket: Sized {
    /// Creates and binds a new socket to the specified address.
    fn bind(addr: SocketAddr) -> impl Future<Output = Result<Self>> + SendOnMt;

    /// Receives a batch of messages.
    ///
    /// The `messages` slice will be filled with incoming packets.
    /// Returns the number of messages successfully read.
    fn recv(&self, messages: &mut [Msg]) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Sends a batch of messages.
    ///
    /// Returns the number of messages successfully flushed to the network.
    fn send(&self, messages: &[Msg]) -> impl Future<Output = Result<usize>> + SendOnMt;

    /// Setup socket to ensure it will run efficiently on current environment.
    fn setup(&self) -> Result<()>;

    /// Try to enable a specific feature.
    fn enable(&self, feature: Feature) -> bool;
}

/// Message to be sent/received.
pub struct Msg {
    /// Buffer to sent, or receive data to.
    pub(crate) buf: BytesMut,

    /// Source address.
    pub(crate) from: Option<SocketAddr>,

    /// Destination address.
    pub(crate) to: Option<SocketAddr>,

    /// Explicit Congestion Notification.
    pub(crate) ecn: Ecn,

    /// GSO/GRO segment size, if enabled.
    pub(crate) segment_size: Option<usize>,

    /// (Recv only): accurate packet timestamp.
    pub(crate) rx_timestamp: Option<Instant>,

    /// (Recv only): marks how many bytes were written.
    pub(crate) rx_mark: usize,
}

impl Msg {
    /// Returns a reference to the underlying buffer data.
    pub fn buf(&self) -> &[u8] {
        self.buf.as_ref()
    }

    /// Returns a mutable reference to the underlying buffer data.
    pub fn buf_mut(&mut self) -> &mut [u8] {
        self.buf.as_mut()
    }

    /// Returns the source address.
    ///
    /// # Panics
    ///
    /// Panics if the source address is not set.
    pub fn from(&self) -> SocketAddr {
        self.from.expect("socket address 'from' is not ready yet")
    }

    /// Returns the destination address.
    ///
    /// # Panics
    ///
    /// Panics if the destination address is not set.
    pub fn to(&self) -> SocketAddr {
        self.to.expect("socket address 'to' is not ready yet")
    }

    /// Returns the Explicit Congestion Notification (ECN) bits.
    pub fn ecn(&self) -> Ecn {
        self.ecn
    }

    /// Returns the GSO/GRO segment size, if enabled.
    pub fn segment_size(&self) -> Option<usize> {
        self.segment_size
    }

    /// Returns the accurate packet timestamp, if available.
    pub fn rx_timestamp(&self) -> Option<Instant> {
        self.rx_timestamp
    }

    /// Returns the number of bytes written to the buffer during receive.
    pub fn rx_mark(&self) -> usize {
        self.rx_mark
    }

    /// Sets the source address.
    pub fn set_from(&mut self, from: SocketAddr) {
        self.from = Some(from);
    }

    /// Sets the destination address.
    pub fn set_to(&mut self, to: SocketAddr) {
        self.to = Some(to);
    }

    /// Sets the Explicit Congestion Notification (ECN) bits.
    pub fn set_ecn(&mut self, ecn: Ecn) {
        self.ecn = ecn;
    }

    /// Sets the GSO/GRO segment size.
    pub fn set_segment_size(&mut self, segment_size: Option<usize>) {
        self.segment_size = segment_size;
    }

    /// Sets the accurate packet timestamp.
    pub fn set_rx_timestamp(&mut self, rx_timestamp: Option<Instant>) {
        self.rx_timestamp = rx_timestamp;
    }

    /// Sets the number of bytes written to the buffer.
    pub fn set_rx_mark(&mut self, rx_mark: usize) {
        self.rx_mark = rx_mark;
    }
}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Feature {
    /// `SO_REUSEPORT` on Linux.
    ReusePort,

    /// `UDP_GRO` on Linux.
    GRO,
}


/// Explicit Congestion Notification.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Ecn {
    /// Not ECN-Capable Transport (00)
    NEct = 0b00,

    /// ECN Capable Transport(1) (01).
    Ect1 = 0b01,

    /// ECN Capable Transport(0) (10),
    Ect0 = 0b10,

    /// Congestion Experienced (11)
    Ce = 0b11,
}

impl Default for Ecn {
    fn default() -> Self {
        Self::NEct
    }
}

impl Display for Ecn {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Ecn::NEct => write!(f, "Not-ECT"),
            Ecn::Ect1 => write!(f, "ECT(1)"),
            Ecn::Ect0 => write!(f, "ECT(0)"),
            Ecn::Ce => write!(f, "CE"),
        }
    }
}
