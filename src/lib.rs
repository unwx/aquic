use crate::executor::SendIfRt;
use crate::stream::codec::{Decoder, DecoderFactory, Encoder, EncoderFactory};
use std::fmt::Debug;
use std::hash::Hash;

mod backend;
mod executor;
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

/// Protocol specification.
pub trait Spec: Debug + Send + 'static {
    /// Item to be decoded or encoded.
    type Item: SendIfRt + 'static;

    /// Protocol error.
    type Error: Error + SendIfRt + 'static;

    /// Stream encoder.
    type Encoder: Encoder<Item = Self::Item> + SendIfRt + 'static;

    /// Stream decoder.
    type Decoder: Decoder<Item = Self::Item> + SendIfRt + 'static;

    /// [Self::Encoder] factory.
    type EncoderFactory: EncoderFactory<Encoder = Self::Encoder>;

    /// [Self::Decoder] factory.
    type DecoderFactory: DecoderFactory<Decoder = Self::Decoder>;


    /// Returns protocol error in case of internal error,
    /// and we need to hang up the communication.
    fn on_hangup() -> Self::Error;

    /// Converts [Self::Encoder] error into protocol [Self::Error].
    fn on_encoder_error(error: &<Self::Encoder as Encoder>::Error) -> Self::Error;

    /// Converts [Self::Decoder] error into protocol [Self::Error].
    fn on_decoder_error(error: &<Self::Decoder as Decoder>::Error) -> Self::Error;
}
