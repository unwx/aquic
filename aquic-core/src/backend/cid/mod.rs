use crate::conditional;
use std::borrow::{Borrow, Cow};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

mod common;
pub use common::*;

conditional! {
    feature = "server-util",

    mod server;
    pub use server::*;
}


/// Maximum connection ID length (RFC 9000).
pub const MAX_CID_LEN: usize = 20;


/// A Connection ID.
///
/// This struct holds raw bytes of a connection ID.
#[derive(Debug, Clone)]
pub struct ConnectionId {
    array: [u8; MAX_CID_LEN],
    len: usize,
}

impl ConnectionId {
    /// Creates a Connection ID from `slice`.
    ///
    /// Returns `None` if the `slice` length is greater than [MAX_CID_LEN].
    #[inline]
    pub fn try_from_slice(slice: &[u8]) -> Option<Self> {
        if slice.len() > MAX_CID_LEN {
            return None;
        }

        let mut array = [0u8; MAX_CID_LEN];
        array[..slice.len()].copy_from_slice(slice);

        Some(Self {
            array,
            len: slice.len(),
        })
    }

    /// Creates a Connection ID from iterator.
    ///
    /// Returns `None` if iterator length is greater than [MAX_CID_LEN].
    #[inline]
    pub fn try_from_iter<I: Iterator<Item = u8>>(mut iter: I) -> Option<Self> {
        let mut array = [0u8; MAX_CID_LEN];

        let mut mark = 0;
        while mark < MAX_CID_LEN {
            match iter.next() {
                Some(byte) => {
                    array[mark] = byte;
                    mark += 1;
                }
                None => {
                    return Some(Self { array, len: mark });
                }
            }
        }

        None
    }

    /// Returns connection ID as an immutable slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.array[..self.len()]
    }

    /// Returns connection ID as a mutable slice.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let length = self.len();
        &mut self.array[..length]
    }

    /// Returns connection ID length.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if connection ID is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl From<[u8; MAX_CID_LEN]> for ConnectionId {
    fn from(value: [u8; MAX_CID_LEN]) -> Self {
        Self {
            len: value.len(),
            array: value,
        }
    }
}

impl AsRef<[u8]> for ConnectionId {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsMut<[u8]> for ConnectionId {
    fn as_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

impl Borrow<[u8]> for ConnectionId {
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for ConnectionId {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }

        self.array[..self.len] == other.array[..other.len]
    }
}

impl Eq for ConnectionId {}

impl Hash for ConnectionId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.array[..self.len].hash(state);
    }
}

impl Display for ConnectionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.as_slice() {
            write!(f, "{byte:02x}")?;
        }

        Ok(())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self {
            array: [0u8; MAX_CID_LEN],
            len: 0,
        }
    }
}


/// The specified Connection ID is invalid.
#[derive(Debug, Clone)]
pub struct IdError {
    pub original: ConnectionId,
    pub detail: Cow<'static, str>,
}

impl Display for IdError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "connection-id '{}' is invalid: {}",
            &self.original, &self.detail
        )
    }
}

impl std::error::Error for IdError {}


/// Metadata that a connection ID holds.
pub trait ConnectionIdMeta {
    /// Returns a CPU Core ID, if present.
    ///
    /// Core ID is **required** for server-side routing in thread-per-core async runtimes like
    /// [monoio](https://github.com/bytedance/monoio),
    /// where multiple sockets (socket per core) bind on the same port.
    ///
    /// Without the Core ID it's impossible to determine which socket, CPU core should process the packet.
    ///
    /// It is not neccessary for client applications, or single-thread/work-stealing async runtimes.
    fn core_id(&self) -> Option<u16> {
        None
    }
}

impl ConnectionIdMeta for () {}


/// A local source connection IDs generator.
pub trait ConnectionIdGenerator: Send {
    // Some QUIC implementations (like quinn-proto) require ConnIdGenerators to be Send + Sync.
    // Therefore, `Send` requirement is mandatory...

    type Meta: ConnectionIdMeta;

    /// Generates a new source connection ID.
    ///
    /// Connection IDs **must not** contain any information that can be used by
    /// an external observer to correlate them with other connection IDs for the same
    /// connection.
    ///
    /// They **must** have high entropy.
    fn generate(&self) -> ConnectionId;

    /// Returns the length of generated connection IDs.
    ///
    /// Must be constant, may be zero.
    fn cid_len(&self) -> usize;

    /// Quickly determines whether `cid` could have been generated by this generator.
    ///
    /// False positives are permitted, but they might increase the cost of handling invalid packets.
    ///
    /// **Must not** produce false negatives.
    fn validate(&self, cid: &ConnectionId) -> bool;

    /// Decrypts a connection ID.
    ///
    /// Noop if it was not encrypted.
    fn decrypt(&self, cid: &mut ConnectionId) -> Result<(), IdError>;

    /// Returns connection ID's metadata.
    fn parse(&self, cid: &ConnectionId) -> Result<Self::Meta, IdError>;
}
