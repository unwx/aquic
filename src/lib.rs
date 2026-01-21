use crate::stream::codec::{Decoder, Encoder};
use std::fmt::Debug;
use std::hash::Hash;

pub mod backend;
mod conn;
mod exec;
mod net;
mod stream;
mod sync;

/// Protocol defined error to be sent inside `STOP_SENDING` or `RESET_STREAM` frames.
#[rustfmt::skip]
pub trait Error:
    std::error::Error +
    Send + Sync + 'static +
    Copy + Eq + Hash +
    From<u64> + Into<u64>
{
}

/// Makes a custom estimation of [Self], can be `zero`.
///
/// Returned value will be used as `weight` in [sync::stream::channel]
/// for the current value - [Self].
///
/// As [sync::stream::channel] is a bounded channel,
/// it needs to know how many items it can hold inside for `Receiver` to consume,
/// until they exceed the `bound`.
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
/// And, with a `bound` of `1024` we can handle many-many `Handshake`/`LargeItem`/`Ping` messages,
/// or `Item` messages until their memory occupation exceeds `1024`.
pub trait Estimate {
    fn estimate(&self) -> usize {
        usize::MAX
    }
}

/// Protocol specification.
pub trait Spec: Debug + SendOnMt + 'static {
    /// Item to be decoded or encoded.
    type Item: Estimate + SendOnMt + Unpin + 'static;

    /// Protocol error.
    type Error: Error + SendOnMt + 'static;

    /// Stream encoder.
    type Encoder: Encoder<Item = Self::Item> + SendOnMt + 'static;

    /// Stream decoder.
    type Decoder: Decoder<Item = Self::Item> + SendOnMt + 'static;


    /// Returns protocol error in case of internal error,
    /// and we need to hang up the communication.
    fn on_hangup() -> Self::Error;

    /// Converts [Self::Encoder] error into protocol [Self::Error].
    fn on_encoder_error(error: &<Self::Encoder as Encoder>::Error) -> Self::Error;

    /// Converts [Self::Decoder] error into protocol [Self::Error].
    fn on_decoder_error(error: &<Self::Decoder as Decoder>::Error) -> Self::Error;


    /// Creates a new [`Encoder`] instance.
    fn new_encoder() -> Self::Encoder;

    /// Creates a new [`Decoder`] instance.
    fn new_decoder() -> Self::Decoder;
}


macro_rules! conditional {
    ($condition:meta, $($item:item)*) => {
        $(
            #[cfg($condition)]
            $item
        )*
    };
}

use crate::exec::SendOnMt;
use conditional;
