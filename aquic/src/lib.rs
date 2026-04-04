#![deny(rustdoc::broken_intra_doc_links)]

use crate::backend::QuicBackend;
use crate::exec::{SendOnMt, SyncOnMt};
use crate::net::{SoFeat, Socket};
use bytes::Bytes;
use std::collections::HashSet;
use std::hash::Hash;
use std::io;
use std::net::{IpAddr, SocketAddr};

pub mod backend;
pub mod dgram;
pub mod net;
pub mod stream;
pub mod sync;
pub mod util;

mod core;
mod exec;
mod tracing;

/// Custom application stream-level error code,
/// that is used for `RESET_STREAM` and `STOP_SENDING` frames.
///
/// `u64` value of the code must not exceed `2^62 - 1` (inclusive),
///
/// More: [20.2. Application Protocol Error Codes](https://datatracker.ietf.org/doc/html/rfc9000#name-application-protocol-error-)
#[rustfmt::skip]
pub trait StreamCode:
    std::error::Error +
    Send + Sync + 'static +
    Copy + Eq + Hash +
    From<u64> + Into<u64>
{}

/// Custom application connection-level error code,
/// that is used for a `CONNECTION_CLOSE` frame.
///
/// `u64` value of the code must not exceed `2^62 - 1` (inclusive),
///
/// More: [20.2. Application Protocol Error Codes](https://datatracker.ietf.org/doc/html/rfc9000#name-application-protocol-error-)
#[rustfmt::skip]
pub trait ConnectionCode:
    std::error::Error +
    Send + Sync + 'static +
    Copy + Eq + Hash +
    From<u64> + Into<u64>
{
    /// Additional diagnostic information for the connection closure.
    /// This can be zero length if the sender chooses not to give details beyond the Error Code value.
    ///
    /// This **should** be a UTF-8 encoded string,
    /// though the frame does not carry information,
    /// such as language tags,
    /// that would aid comprehension by any entity other than the one that created the text.
    ///
    /// More: [19.19. CONNECTION_CLOSE Frames, Reason Phrase](https://datatracker.ietf.org/doc/html/rfc9000#name-connection_close-frames)
    fn reason(self) -> Bytes;
}


/// Specification of the custom protocol,
/// built on top of QUIC.
pub trait Spec: SendOnMt + SyncOnMt + 'static {
    /// Stream communication primitive: an item to be decoded and encoded, sent and received.
    type StreamItem: Estimate + SendOnMt + Unpin + 'static;

    /// Datagram extension communication primitive: an item to be decoded and encoded, sent and received.
    type DgramItem: Estimate + SendOnMt + Unpin + 'static;

    /// Stream error code.
    type StreamCode: StreamCode;

    /// Connection error code.
    type ConnectionCode: ConnectionCode;

    // TODO(feat): encoder/decoder error handling.


    /// Stream encoder.
    type StreamEncoder: stream::Encoder<Item = Self::StreamItem, Error = Self::StreamEncoderError>
        + SendOnMt
        + 'static;

    /// Stream decoder.
    type StreamDecoder: stream::Decoder<Item = Self::StreamItem, Error = Self::StreamDecoderError>
        + SendOnMt
        + 'static;

    /// Datagram encoder.
    type DgramEncoder: dgram::Encoder<Item = Self::DgramItem> + SendOnMt + 'static;

    /// Datagram decoder.
    type DgramDecoder: dgram::Decoder<Item = Self::DgramItem> + SendOnMt + 'static;


    /// [Spec::StreamEncoder] error type.
    type StreamEncoderError: std::error::Error + Clone + Send + Sync + 'static;

    /// [Spec::StreamDecoder] error type.
    type StreamDecoderError: std::error::Error + Clone + Send + Sync + 'static;


    /// Creates a new [`stream::Encoder`] instance.
    fn new_stream_encoder(&self) -> Self::StreamEncoder;

    /// Creates a new [`stream::Decoder`] instance.
    fn new_stream_decoder(&self) -> Self::StreamDecoder;

    /// Creates a new [`dgram::Encoder`] instance.
    fn new_datagram_encoder(&self) -> Self::DgramEncoder;

    /// Creates a new [`dgram::Decoder`] instance.
    fn new_datagram_decoder(&self) -> Self::DgramDecoder;


    /// The maximum capacity of the stream channel.
    ///
    /// **Note**: each item inside the stream channel might occupy available space differently,
    /// depending on item's [`Estimate`] implementation.
    fn stream_channel_bound(&self) -> usize;

    /// The maximum capacity of the dgram channel.
    ///
    /// **Note**: each item inside the datagram channel might occupy available space differently,
    /// depending on item's [`Estimate`] implementation.
    fn datagram_channel_bound(&self) -> usize;
}


/// Everything that [`protocol`](Spec) needs to work on the network level.
pub trait Engine: SendOnMt + SyncOnMt + 'static {
    /// Socket to use for network I/O.
    type Socket: Socket;

    /// QUIC protocol implementation provider.
    type QuicBackend: QuicBackend;

    /// Creates a new socket and binds it to the specified address.
    ///
    /// Implementation may customize socket options before returning it.
    fn bind_socket(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Socket>>;

    /// Creates a new [`QuicBackend`] instance.
    fn new_quic_backend(
        &self,
        listen_addr: IpAddr,
        socket_features: &HashSet<SoFeat>,
    ) -> Self::QuicBackend;
}


/// Makes a custom estimation of [Self], which can be `zero`.
///
/// # What for
///
/// Bounded stream/datagram channels:
/// - [sync::stream].
/// - [sync::dgram].
///
/// As these channels are bounded,
/// they need to know how many items they can hold inside for a `Receiver` to consume,
/// until internal buffer exceeds the `bound` and blocks further `send()` attempts.
///
/// These channels don't just count the number of items,
/// they allow for each item to have its own weight (you can call it size, volume, cost, etc).
///
/// Therefore, using this estimation method, it is possible to achieve something like this:
///
/// ```no_run
///   # use crate::Estimate;
///   use std::path::Path;
///
///   enum Msg {
///       Handshake(String),
///       Ping,
///       Item(Vec<u8>),
///       LargeItem(Path),
///       Bye,
///   }
///
///   impl Estimate for Msg {
///       fn estimate(&self) -> usize {
///           // Let's estimate a [Msg] by its approximate footprint on RAM.
///
///           // We can do any other estimation,
///           // but memory usage restriction is the most basic one.
///
///           // We can do this accurately,
///           // but for example purposes simple is enough.
///
///           match self {
///               Self::Handshake(id) => id.len(),
///               Self::Item(payload) => payload.len(),
///               Self::LargeItem(path) => path.as_os_str().len(), // Item is stored on disk, not on RAM.
///               Self::Ping => 1,
///               Self::Bye => 0,
///           }
///       }
///   }
/// ```
///
/// Therefore, with a `bound` of `1024` we can handle a lot `Handshake`/`LargeItem`/`Ping` messages,
/// or limited number of `Item` messages until their memory occupation exceeds ~`1024`.
pub trait Estimate {
    /// Returned value can be any value, including `zero` or `usize::MAX`.
    fn estimate(&self) -> usize;
}

impl<T: AsRef<[u8]>> Estimate for T {
    fn estimate(&self) -> usize {
        self.as_ref().len()
    }
}


/// Conditional code:
/// ```no_run
/// conditional! {
///     any(feature = "feat"),
///
///     fn something() {
///     }
///
///     struct Special;
/// }
/// ```
macro_rules! conditional {
    ($condition:meta, $($item:item)*) => {
        $(
            #[cfg($condition)]
            $item
        )*
    };
}

/// Panics if `debug_assertions` is enabled.
macro_rules! debug_panic {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            panic!($($arg)*);
        }
    };
}

/// Write a log with a dynamic log level.
macro_rules! log {
    ($lvl:expr, $($arg:tt)+) => {
        match $lvl {
            tracing::Level::ERROR => tracing::error!($($arg)+),
            tracing::Level::WARN  => tracing::warn!($($arg)+),
            tracing::Level::INFO  => tracing::info!($($arg)+),
            tracing::Level::DEBUG => tracing::debug!($($arg)+),
            tracing::Level::TRACE => tracing::trace!($($arg)+),
        }
    }
}

use conditional;
use debug_panic;
use log;
