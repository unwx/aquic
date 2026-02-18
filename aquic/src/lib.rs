use crate::backend::QuicBackend;
use crate::exec::{SendOnMt, SyncOnMt};
use crate::net::{SoFeat, Socket};
use bytes::{Bytes, BytesMut};
use std::collections::HashSet;
use std::hash::Hash;
use std::io;
use std::net::SocketAddr;


pub mod backend;
pub mod dgram;
pub mod net;
pub mod stream;
pub mod sync;

mod core;
mod exec;
mod tracing;

/// Custom application error.
///
/// Application protocol error codes are used for
/// the `RESET_STREAM` frame,
/// the `STOP_SENDING` frame, and
/// the `CONNECTION_CLOSE` frame.
///
/// Make sure its `u64` representation is in range of `[0..2^62]`,
/// as higher values might lead to panics ([quinn_proto::VarInt::MAX]).
///
/// More: [20.2. Application Protocol Error Codes](https://datatracker.ietf.org/doc/html/rfc9000#name-application-protocol-error-)
#[rustfmt::skip]
pub trait Error:
    std::error::Error +
    Send + Sync + 'static +
    Copy + Eq + Hash +
    From<u64> + Into<u64>
{
    /// Additional diagnostic information for the **connection** closure.
    /// This can be zero length if the sender chooses not to give details beyond the Error Code value.
    ///
    /// This **should** be a UTF-8 encoded string,
    /// though the frame does not carry information,
    /// such as language tags,
    /// that would aid comprehension by any entity other than the one that created the text.
    ///
    /// Note: This reason is only transmitted in `CONNECTION_CLOSE` frames,
    /// and also may be truncated in some circumstances.
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

    /// Custom application error.
    type Error: Error;


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


    /// Returns an application error on internal error, such as:
    /// - [`stream::Sender`](sync::stream::Sender) was dropped because of panic.
    ///
    /// - [`backend::QuicEndpoint`] provider has a bug, or this library has a bug
    ///   leading to unrecoverable connection state.
    ///
    /// This error is used to notify the peer about the incident, when possible.
    fn on_hangup() -> Self::Error;

    /// Converts [`stream::Encoder`] error into application error.
    fn on_stream_encoder_error(error: &Self::StreamEncoderError) -> Self::Error;

    /// Converts [`stream::Decoder`] error into application error.
    fn on_stream_decoder_error(error: &Self::StreamDecoderError) -> Self::Error;


    /// Creates a new [`stream::Encoder`] instance.
    fn new_stream_encoder(&self) -> Self::StreamEncoder;

    /// Creates a new [`stream::Decoder`] instance.
    fn new_stream_decoder(&self) -> Self::StreamDecoder;

    /// Creates a new [`dgram::Encoder`] instance.
    fn new_dgram_encoder(&self) -> Self::DgramEncoder;

    /// Creates a new [`dgram::Decoder`] instance.
    fn new_dgram_decoder(&self) -> Self::DgramDecoder;


    /// Returns a total number of encoded bytes,
    /// that is enough to flush the encoded output to the network.
    ///
    /// May be `zero` for an immediate effect.
    ///
    /// **Notes**:
    /// - it has effect only if there are pending items to be encoded:
    ///   it won't block & wait for new items.
    /// - it is recommended to set `QuicBackend` stream buffer size larger than `stream_encoder_max_batch_size`
    ///   to avoid partial writes bottleneck.
    /// - setting a large value might not be efficient, as ideally batch size should not exceed peer's flow control limit.
    fn stream_encoder_max_batch_size(&self) -> usize;

    /// Returns a maximum total number of bytes,
    /// that is allowed to pass in a single [`decode()`][stream::Decoder::decode] call.
    ///
    /// **Note**: it is recommended to set [`stream_channel_bound`][Spec::stream_channel_bound] accordingly,
    /// as each decoded item goes into the channel afterwards.
    fn stream_decoder_max_batch_size(&self) -> usize;

    /// Returns a total number of encoded bytes,
    /// that is enough to flush the encoded output to the network.
    ///
    /// May be `zero` for an immediate effect.
    ///
    /// **Notes**:
    /// - it has effect only if there are pending items to be encoded:
    ///   it won't block & wait for new items.
    /// - it is recommended to set `QuicBackend` datagram buffer size larger than `dgram_encoder_max_batch_size`
    ///   to avoid partial writes bottleneck (or outgoing packet loss).
    /// - setting a large value might not be efficient, as peer might drop packets exceeding his buffer capacity.
    fn dgram_encoder_max_batch_size(&self) -> usize;

    /// Returns a maximum desired total number of bytes,
    /// that should to pass in a single [`decode()`][dgram::Decoder::decode] call.
    ///
    /// May be `zero`, to allow only a single datagram.
    ///
    /// **Notes**:
    /// - it is recommended to set [`dgram_channel_bound`][Spec::dgram_channel_bound] accordingly,
    ///   as each decoded item goes into the channel afterwards, and **will be dropped** if channel is full.
    /// - real batch size may exceed the specified desired value, up to a single extra datagram.
    fn dgram_decoder_max_batch_size(&self) -> usize;

    /// The maximum capacity of the stream channel.
    ///
    /// **Note**: each item inside the stream channel might occupy available space differently,
    /// depending on item's [`Estimate`] implementation.
    fn stream_channel_bound(&self) -> usize;

    /// The maximum capacity of the dgram channel.
    ///
    /// **Note**: each item inside the datagram channel might occupy available space differently,
    /// depending on item's [`Estimate`] implementation.
    fn dgram_channel_bound(&self) -> usize;
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
        socket_addr: SocketAddr,
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

macro_rules! impl_estimate {
    ($($t:ty),+ $(,)?) => {
        $(
            impl Estimate for $t {
                fn estimate(&self) -> usize {
                    self.len()
                }
            }
        )+
    };
}

impl_estimate!(Bytes, BytesMut, Vec<u8>, [u8], &[u8]);


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
