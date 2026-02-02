use std::{
    fmt::{Display, Formatter},
    io::{IoSlice, IoSliceMut},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    str::FromStr,
    time::Instant,
};

use domain::base::name::UncertainName;

/// Maximum UDP packet size + 1, to make it power of two.
pub const MAX_PACKET_SIZE: usize = 65536;

/// A Socket Feature.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SoFeat {
    /// Permits multiple sockets to be bound to an identical socket address.
    ReusePort,

    /// Socket will return an error if packet size exceeds MTU.
    DontFragment,

    /// This socket supports `sendmmsg/recvmmsg` syscalls (or their alternative).
    ///
    /// Or the socket will simply benefit receiving messages in batches.
    Mmsg,

    /// Generic Receive Offload.
    ///
    /// If enabled, the socket **should** receive multiple datagrams worth of data as a single large buffer.
    /// [RecvMsg::segment_size] will be `non-zero` in such scenario.
    ///
    /// **Note**: a single GRO datagram **must not** include different source addresses,
    /// but **may** contain QUIC packets from different connections (with different Connection ID).
    GenericRecvOffload,

    /// Generic Segmentation Offload.
    ///
    /// If enabled, the socket **should** send multiple datagrams worth of data as a single large buffer.
    /// [SendMsg::segment_size] will be `non-zero` in such scenario.
    GenericSegOffload,
}


/// A single UDP datagram to be received.
///
/// **Note**: **may** contain QUIC packets from different connections (with different Connection ID)
/// **only if** `segment_size` is `Some`.
pub struct RecvMsg<B> {
    buf: B,
    read: usize,

    from: SocketAddr,
    to: IpAddr,

    ecn: Ecn,
    segment_size: usize,
    timestamp: Option<Instant>,
}

impl<B: BufMut> RecvMsg<B> {
    /// Create a new blank message.
    ///
    /// # Panics
    ///
    /// If provided buffer is empty.
    #[inline]
    pub fn new(buf: B) -> Self {
        assert!(buf.capacity() > 0, "provided slice capacity must be > 0");

        Self {
            buf,
            read: 0,
            from: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0)),
            to: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            ecn: Ecn::NEct,
            segment_size: 0,
            timestamp: None,
        }
    }

    /// Update the message with received fields.
    ///
    /// * `read`: how many bytes were written into the slice.
    /// * `from`: source address.
    /// * `to`: destination address.
    /// * `ecn`: ECN.
    /// * `segment_size`: `non-zero` if the packet is segmented with GRO.
    /// * `timestamp`: when a kernel received packet, if present.
    ///
    /// # Panics
    ///
    /// If `from` or `to` address is unspecified.
    /// Please ignore such packets.
    #[inline]
    pub fn recv(
        &mut self,
        read: usize,
        from: SocketAddr,
        to: IpAddr,
        ecn: Ecn,
        segment_size: usize,
        timestamp: Option<Instant>,
    ) {
        {
            let unspecified = match from {
                SocketAddr::V4(v4) => v4.ip().is_unspecified() || v4.port() == 0,
                SocketAddr::V6(v6) => v6.ip().is_unspecified() || v6.port() == 0,
            };

            assert!(
                !unspecified,
                "packet source address is unspecified and might be malicious: {}",
                from
            );
        }

        assert!(
            !to.is_unspecified(),
            "packet destination address is unspecified and might be malicious: {}",
            to
        );

        if cfg!(debug_assertions) {
            match (from, to) {
                (SocketAddr::V4(_), IpAddr::V4(_)) | (SocketAddr::V6(_), IpAddr::V6(_)) => {
                    // OK.
                }
                (from, to) => {
                    panic!(
                        "`source`({from}) and `destination`({to}) address must share the same address family"
                    );
                }
            }
        }

        self.read = read;
        self.from = from;
        self.to = to;
        self.ecn = ecn;
        self.segment_size = segment_size;
        self.timestamp = timestamp;
    }

    /// Returns `true` if this message was filled with packet's data.
    #[inline]
    pub fn has_data(&self) -> bool {
        self.read != 0
    }

    /// Returns a slice that is intended for writing a data received from peer.
    #[inline]
    pub(crate) fn slice_write(&mut self) -> IoSliceMut<'_> {
        debug_assert!(!self.has_data());
        self.buf.as_mut_write_io_slice()
    }

    /// Returns a slice that is intended for reading the received data.
    #[inline]
    pub fn slice_read(&mut self) -> &mut [u8] {
        debug_assert!(self.has_data());
        self.buf.as_mut_read_slice()
    }

    /// Returns `source` address.
    #[inline]
    pub fn from(&self) -> SocketAddr {
        debug_assert!(self.has_data());
        self.from
    }

    /// Returns `to` address.
    #[inline]
    pub fn to(&self) -> IpAddr {
        debug_assert!(self.has_data());
        self.to
    }

    /// Returns packet ECN.
    #[inline]
    pub fn ecn(&self) -> Ecn {
        debug_assert!(self.has_data());
        self.ecn
    }

    /// Returns GRO segment size, or `zero` if the packet was not segmented.
    #[inline]
    pub fn segment_size(&self) -> usize {
        debug_assert!(self.has_data());
        self.segment_size
    }

    /// Returns a time when a kernel received the packet, or `default`.
    #[inline]
    pub fn timestamp(&self, default: Instant) -> Instant {
        debug_assert!(self.has_data());
        self.timestamp.unwrap_or(default)
    }

    /// Transforms into the original buffer.
    #[inline]
    pub fn into_buf(self) -> B {
        self.buf
    }
}


/// A single UDP datagram to be sent.
///
/// **Note**:
/// Receivers **may** route based on the information in the first packet contained in a UDP datagram.
///
/// Therefore, Senders **must not** coalesce QUIC packets with different connection IDs into a single UDP datagram.
///
/// More: [12.2. Coalescing Packets](https://datatracker.ietf.org/doc/html/rfc9000#name-coalescing-packets)
#[derive(Copy, Clone)]
pub struct SendMsg<B> {
    buf: B,
    from: IpAddr,
    to: SocketAddr,
    ecn: Ecn,
    segment_size: usize,
}

impl<B: Buf> SendMsg<B> {
    /// Creates a new message.
    ///
    /// * `buf`: buffer with the data to be sent.
    /// * `from`: source address.
    /// * `to`: destination address.
    /// * `ecn`: ECN.
    /// * `segment_size`: `non-zero` if packet is segmented for GSO.
    ///
    /// # Panics
    ///
    /// - If provided `buf` is empty.
    /// - If `to` address is unspecified.
    #[inline]
    pub fn new(buf: B, from: IpAddr, to: SocketAddr, ecn: Ecn, segment_size: usize) -> Self {
        assert!(buf.len() != 0, "provided buf must not be empty");

        {
            let unspecified = match to {
                SocketAddr::V4(v4) => v4.ip().is_unspecified() || v4.port() == 0,
                SocketAddr::V6(v6) => v6.ip().is_unspecified() || v6.port() == 0,
            };

            assert!(
                !unspecified,
                "packet destination address must be specified: {}",
                to
            );
        }

        if cfg!(debug_assertions) {
            match (from, to) {
                (IpAddr::V4(_), SocketAddr::V4(_)) | (IpAddr::V6(_), SocketAddr::V6(_)) => {
                    // OK.
                }
                (from, to) => {
                    panic!(
                        "`source`({from}) and `destination`({to}) address must share the same address family"
                    );
                }
            }
        }

        Self {
            buf,
            from,
            to,
            ecn,
            segment_size,
        }
    }

    /// Slice that contains data to be sent.
    #[inline]
    pub fn io_slice(&self) -> IoSlice<'_> {
        self.buf.as_read_io_slice()
    }

    /// Source address.
    #[inline]
    pub fn from(&self) -> IpAddr {
        self.from
    }

    /// Destination address.
    #[inline]
    pub fn to(&self) -> SocketAddr {
        self.to
    }

    /// Explicit Congestion Notification.
    #[inline]
    pub fn ecn(&self) -> Ecn {
        self.ecn
    }

    /// `non-zero` segment size, if the slice is segmented.
    #[inline]
    pub fn segment_size(&self) -> usize {
        self.segment_size
    }

    /// Converts `SendMsg` into the original buffer it was created with.
    #[inline]
    pub fn into_buf(self) -> B {
        self.buf
    }
}


/// Explicit Congestion Notification.
#[repr(u8)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Ecn {
    /// Not ECN-Capable Transport (00)
    #[default]
    NEct = 0b00,

    /// ECN Capable Transport(1) (01).
    Ect1 = 0b01,

    /// ECN Capable Transport(0) (10),
    Ect0 = 0b10,

    /// Congestion Experienced (11)
    Ce = 0b11,
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


/// Invalid [ServerName].
#[derive(Debug, Copy, Clone)]
pub struct ServerNameError;

impl Display for ServerNameError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "provided server name is neither valid DNS name nor valid IP address",
        )
    }
}

impl std::error::Error for ServerNameError {}


/// Server name, which can be a relative or absolute Domain Name,
/// or simply an IPv4/IPv6 address.
///
/// Usage example:
/// ```no_run
/// let name = ServerName::try_from("server.com").unwrap();
/// let other_name = ServerName::try_from("192.168.1.100").unwrap();
/// println!("{}", name);
/// ```
#[derive(Clone)]
pub enum ServerName {
    Domain(UncertainName<Vec<u8>>),
    Ip(IpAddr),
}

impl TryFrom<&str> for ServerName {
    type Error = ServerNameError;

    fn try_from(value: &str) -> std::result::Result<Self, Self::Error> {
        if let Ok(name) = UncertainName::from_str(value) {
            return Ok(Self::Domain(name));
        }

        if let Ok(addr) = IpAddr::from_str(value) {
            return Ok(Self::Ip(addr));
        }

        Err(ServerNameError)
    }
}

impl Display for ServerName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(it) => write!(f, "{}", it),
            Self::Ip(it) => write!(f, "{}", it),
        }
    }
}


/// A read-only buffer.
pub trait Buf {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn capacity(&self) -> usize;

    /// Returns `[..len()]` slice for reading.
    fn as_read_slice(&self) -> &[u8];

    /// Returns `[..len()]` I/O slice for reading.
    fn as_read_io_slice(&self) -> IoSlice<'_>;
}

/// A mutable buffer.
pub trait BufMut: Buf {
    /// Returns `[..len()]` slice for reading (and mutating).
    fn as_mut_read_slice(&mut self) -> &mut [u8];

    /// Returns `[..capacity()]` slice for writing.
    fn as_mut_write_io_slice(&mut self) -> IoSliceMut<'_>;
}
