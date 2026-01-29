use crate::backend::QuicBackend;
use crate::exec::{SendOnMt, SyncOnMt};
use crate::net::{SoFeat, Socket};
use crate::stream::codec::{Decoder, Encoder};
use bytes::Bytes;
use std::collections::HashSet;
use std::hash::Hash;
use std::io;
use std::net::SocketAddr;


pub mod backend;
pub mod net;
pub mod stream;
pub mod sync;


mod core;
mod exec;

/// Custom application error.
///
/// Application protocol error codes are used for
/// the `RESET_STREAM` frame,
/// the `STOP_SENDING` frame, and
/// the `CONNECTION_CLOSE` frame.
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

/// Makes a custom estimation of [Self], which can be `zero`.
///
/// Returned value will be used as `weight` in [sync::stream::channel]
/// for the current value.
///
/// # What for
///
/// As [sync::stream::channel] is a bounded channel,
/// it needs to know how many items it can hold inside for `Receiver` to consume,
/// until it exceeds the `bound`.
///
/// [sync::stream::channel] doesn't just count the number of items,
/// it allows for each item to has its own weight (size, volume, cost, etc).
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
///               Self::LargeItem(path) => path.as_os_str().len(),
///               Self::Ping => 1,
///               Self::Bye => 1,
///           }
///       }
///   }
/// ```
///
/// Therefore, with a `bound` of `1024` we can handle a lot `Handshake`/`LargeItem`/`Ping` messages,
/// and limited `Item` messages until their memory occupation exceeds ~`1024`.
pub trait Estimate {
    fn estimate(&self) -> usize {
        usize::MAX
    }
}

/// Specification of the custom protocol,
/// built on top of QUIC.
pub trait Spec: SendOnMt + SyncOnMt + 'static {
    /// Stream communication primitive: an item to be decoded or encoded, sent or received.
    type Item: Estimate + SendOnMt + Unpin + 'static;

    /// Custom application error.
    type Error: Error;

    /// Stream encoder.
    type Encoder: Encoder<Item = Self::Item> + SendOnMt + 'static;

    /// Stream decoder.
    type Decoder: Decoder<Item = Self::Item> + SendOnMt + 'static;


    /// Returns an application error on internal error, such as:
    /// - [`stream::Sender`](sync::stream::Sender) was dropped because of panic.
    ///
    /// - [`backend::QuicEndpoint`] provider has a bug, or this library has a bug
    ///   leading to unrecoverable connection state.
    ///
    /// This error is used to notify the peer about the incident, when possible.
    fn on_hangup() -> Self::Error;

    /// Converts [`Encoder`] error into application error.
    fn on_encoder_error(error: &<Self::Encoder as Encoder>::Error) -> Self::Error;

    /// Converts [`Decoder`] error into application error.
    fn on_decoder_error(error: &<Self::Decoder as Decoder>::Error) -> Self::Error;


    /// Creates a new [`Encoder`] instance.
    fn new_encoder(&self) -> Self::Encoder;

    /// Creates a new [`Decoder`] instance.
    fn new_decoder(&self) -> Self::Decoder;

    /// The maximum capacity of the stream channel.
    ///
    /// **Note**: please make sure to get acquainted with [`Estimate`],
    /// as each item inside the stream channel might occupy available space differently.
    fn stream_channel_bound(&self) -> usize;
}


/// Everything that [`protocol`](Spec) needs to work on the network level.
pub trait Engine<S: Spec>: SendOnMt + SyncOnMt + 'static {
    /// Socket to use for network I/O.
    type Socket: Socket;

    /// QUIC protocol implementation provider.
    type QuicBackend<'io>: QuicBackend<'io, S>;

    /// Creates a new socket and binds it to the specified address.
    ///
    /// Implementation may customize socket options before returning it.
    fn bind_socket(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Socket>>;

    /// Creates a new [`QuicBackend`] instance.
    fn new_quic_backend<'io>(
        &self,
        so_addr: SocketAddr,
        so_features: &HashSet<SoFeat>,
    ) -> Self::QuicBackend<'io>;
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

use conditional;
