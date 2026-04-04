use crate::net::{Buf, BufMut};
use domain::base::name::UncertainName;
use std::{
    fmt::{Display, Formatter},
    io::{IoSlice, IoSliceMut},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    ops::{Deref, DerefMut},
    str::FromStr,
    time::Instant,
};

/// Maximum UDP packet size.
pub const MAX_PACKET_SIZE: usize = u16::MAX as usize;

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
    ///
    /// **Note**: a single GRO datagram **must not** include different source addresses,
    /// but **may** contain QUIC packets from different connections (with different Connection ID).
    GenericRecvOffload,

    /// Generic Segmentation Offload.
    ///
    /// If enabled, the socket **should** send multiple datagrams worth of data as a single large buffer.
    GenericSegOffload,
}


/// A single UDP message to be received.
///
/// May contain a large GRO packet.
pub struct RecvMsg<B> {
    buf: B,
    read: usize,

    from: SocketAddr,
    to: IpAddr,

    ecn: Ecn,
    segment_count: usize,
    timestamp: Option<Instant>,
}

impl<B: BufMut> RecvMsg<B> {
    /// Create a new blank message.
    ///
    /// # Panics
    ///
    /// If the provided buffer has no capacity for an incoming packet.
    #[inline]
    pub fn new(buf: B) -> Self {
        assert!(
            buf.capacity() > 0,
            "provided 'buf' must have capacity to contain an incoming packet"
        );

        Self {
            buf,
            read: 0,
            from: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0)),
            to: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            ecn: Ecn::NEct,
            segment_count: 0,
            timestamp: None,
        }
    }

    /// Updates the message with received fields.
    ///
    /// * `read`: how many bytes were written into the slice.
    /// * `from`: source address.
    /// * `to`: destination address.
    /// * `ecn`: Explicit Congestion Notification.
    /// * `segment_count`: number of GRO segments,
    ///   `> 1` the msg holds multiple datagrams worth of data as a single large buffer.
    /// * `timestamp`: when the kernel received the packet.
    ///
    /// # Panics
    ///
    /// - If `from` or `to` address is unspecified.
    /// - If `segment_count > 1 && read % segment_count != 0`.
    #[inline]
    pub fn recv(
        &mut self,
        read: usize,
        from: SocketAddr,
        to: IpAddr,
        ecn: Ecn,
        segment_count: usize,
        timestamp: Option<Instant>,
    ) {
        {
            let unspecified = match from {
                SocketAddr::V4(v4) => v4.ip().is_unspecified() || v4.port() == 0,
                SocketAddr::V6(v6) => v6.ip().is_unspecified() || v6.port() == 0,
            };

            assert!(
                !unspecified,
                "packet source address is unspecified: {}",
                from
            );
        }

        assert!(
            !to.is_unspecified(),
            "packet destination address is unspecified: {}",
            to
        );

        if segment_count > 1 {
            assert!(
                read.is_multiple_of(segment_count),
                "'read' length ({read}) must be a multiple of segment count ({segment_count})",
            )
        }

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
        self.segment_count = segment_count;
        self.timestamp = timestamp;
    }

    /// Returns `true` if this message contains useful payload.
    #[inline]
    pub fn has_packet(&self) -> bool {
        self.read != 0
    }

    /// Returns a slice that is intended for writing data received from peer.
    #[inline]
    pub fn write_slice(&mut self) -> IoSliceMut<'_> {
        debug_assert!(!self.has_packet());
        self.buf.as_io_slice()
    }

    /// Returns a slice that is intended for reading data received from peer.
    pub fn read_slice(&mut self) -> &mut [u8] {
        debug_assert!(self.has_packet());
        &mut self.buf.as_slice()[..self.read]
    }

    /// Returns `from` source address.
    #[inline]
    pub fn from(&self) -> SocketAddr {
        debug_assert!(self.has_packet());
        self.from
    }

    /// Returns `to` destination address.
    #[inline]
    pub fn to(&self) -> IpAddr {
        debug_assert!(self.has_packet());
        self.to
    }

    /// Returns packet Explicit Congestion Notification.
    #[inline]
    pub fn ecn(&self) -> Ecn {
        debug_assert!(self.has_packet());
        self.ecn
    }

    /// Returns GRO segment size,
    /// or full packet's length if [`segment_count()`][Self::segment_count] is `<= 1`.
    #[inline]
    pub fn segment_size(&self) -> usize {
        debug_assert!(self.has_packet());

        if self.segment_count == 0 {
            return self.read;
        }

        self.read / self.segment_count
    }

    /// Returns number of GRO segments this single packet holds.
    pub fn segment_count(&self) -> usize {
        debug_assert!(self.has_packet());
        self.segment_count
    }

    /// Returns number of bytes read from network.
    pub fn len(&self) -> usize {
        debug_assert!(self.has_packet());
        self.read
    }

    /// Returns a time when the kernel received the packet.
    #[inline]
    pub fn timestamp(&self) -> Option<Instant> {
        debug_assert!(self.has_packet());
        self.timestamp
    }

    /// Transforms into the original buffer.
    #[inline]
    pub fn into_buf(self) -> B {
        self.buf
    }
}


/// A single UDP datagram to be sent.
///
/// **Notes**:
/// - Receivers **may** route based on the information in the first packet contained in a UDP datagram.
///   Therefore, Senders **must not** coalesce QUIC packets with different connection IDs into a single UDP datagram.
///
///   More: [12.2. Coalescing Packets](https://datatracker.ietf.org/doc/html/rfc9000#name-coalescing-packets)
///
/// - Senders must not combine packets with different ECN into a single GSO message:
///   multiple messages, each with unique ECN must be formed.
#[derive(Copy, Clone)]
pub struct SendMsg<B> {
    buf: B,
    from: IpAddr,
    to: SocketAddr,
    ecn: Ecn,
    segment_count: usize,
}

impl<B: Buf> SendMsg<B> {
    /// Creates a new message.
    ///
    /// * `buf`: buffer with the data to be sent.
    /// * `from`: source address.
    /// * `to`: destination address.
    /// * `ecn`: Explicit Congestion Notification.
    /// * `segment_count`: number of GSO segments (`> 1` if segmented for GSO).
    ///
    /// # Panics
    ///
    /// - If provided `buf` is empty.
    /// - If `to` address is unspecified.
    /// - If `segment_count > 1 && buf.len() % segment_count != 0`.
    #[inline]
    pub fn new(buf: B, from: IpAddr, to: SocketAddr, ecn: Ecn, segment_count: usize) -> Self {
        assert!(buf.len() != 0, "provided 'buf' must not be empty");

        if segment_count > 1 {
            assert!(
                buf.len().is_multiple_of(segment_count),
                "buffer length ({}) must be a multiple of segment count ({})",
                buf.len(),
                segment_count
            )
        }

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
            segment_count,
        }
    }

    /// Returns a slice that contains data to be sent.
    #[inline]
    pub fn io_slice(&self) -> IoSlice<'_> {
        self.buf.as_io_slice()
    }

    /// Returns a source address.
    #[inline]
    pub fn from(&self) -> IpAddr {
        self.from
    }

    /// Returns a destination address.
    #[inline]
    pub fn to(&self) -> SocketAddr {
        self.to
    }

    /// Returns Explicit Congestion Notification value.
    #[inline]
    pub fn ecn(&self) -> Ecn {
        self.ecn
    }

    /// Returns number of GSO segments (`> 1` if segmented for GSO).
    #[inline]
    pub fn segment_count(&self) -> usize {
        self.segment_count
    }

    /// Converts `SendMsg` into the original buffer it was created with.
    #[inline]
    pub fn into_buf(self) -> B {
        self.buf
    }
}


/// An iterator over `&mut [RecvMsg<B>]`,
/// providing to consumer singular packets (or segments)
/// instead of an entire message, that may hold multiple packets inside in case of GRO.
pub struct MultiMsgFlattenIter<'a, B> {
    msgs: &'a mut [RecvMsg<B>],
    msg_index: usize,
    msg_offset: usize,
}

impl<'a, B: BufMut> MultiMsgFlattenIter<'a, B> {
    /// Creates a new iterator on the provided slice.
    pub fn new(msgs: &'a mut [RecvMsg<B>]) -> Self {
        Self {
            msgs,
            msg_index: 0,
            msg_offset: 0,
        }
    }

    /// Returns a single UDP packet (or GRO segment),
    /// or `None` if no more packets available.
    pub fn peek(&mut self) -> Option<Packet<'_>> {
        while self.msg_index < self.msgs.len() {
            {
                if self.msg_offset >= self.msgs[self.msg_index].read {
                    self.msg_index += 1;
                    self.msg_offset = 0;
                    continue;
                }
            }

            let msg = &mut self.msgs[self.msg_index];
            debug_assert_eq!(msg.read % msg.segment_size(), 0);
            debug_assert_eq!(msg.read % self.msg_offset, 0);

            return Some(Packet {
                from: msg.from,
                to: msg.to,
                ecn: msg.ecn,
                timestamp: msg.timestamp,
                payload: {
                    let range = self.msg_offset..(self.msg_offset + msg.segment_size());
                    &mut msg.buf.as_slice()[range]
                },
            });
        }

        None
    }

    /// Tells the iterator to move on to the next packet.
    pub fn next(&mut self) {
        let Some(msg) = self.msgs.get(self.msg_index) else {
            return;
        };

        self.msg_offset += msg.segment_size();
        if self.msg_offset >= msg.read {
            self.msg_index += 1;
            self.msg_offset = 0;
        }

        debug_assert_eq!(msg.read % msg.segment_size(), 0);
        debug_assert_eq!(msg.read % self.msg_offset, 0);
    }
}

/// A single, individual packet UDP packet.
///
/// May never contain GRO segments inside,
/// but may be one of these segments.
#[derive(Debug)]
pub struct Packet<'a> {
    /// Source address, always defined.
    pub from: SocketAddr,

    /// Destination address, always defined.
    pub to: IpAddr,

    /// Packet's Explicit Congestion Notification.
    pub ecn: Ecn,

    /// Packet's timestamp set by the kernel.
    pub timestamp: Option<Instant>,

    /// Underlying data.
    pub payload: &'a mut [u8],
}

impl<'a> Deref for Packet<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.payload
    }
}

impl<'a> DerefMut for Packet<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.payload
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
