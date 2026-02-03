use crate::exec::SendOnMt;
use crate::stream::{Chunk, Payload};
use aquic_macros::supports;
use bytes::Bytes;
use std::future::Future;

/// A QUIC connection consists of multiple streams where data flows.
///
/// [`Decoder`] is a stateful object responsible for a single incoming QUIC stream.
/// Its primary goal is to decode the raw byte stream into meaningful entities of type [`Self::Item`].
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations).
///
/// The library is optimized for both blocking and non-blocking implementations.
#[supports]
pub trait Decoder {
    /// The type of the decoded entity.
    type Item;

    /// The type of error in case of decoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error;

    /// Returns `true` if the decoder supports an unordered stream.
    ///
    /// If a current [`QUIC Backend`][crate::backend::QuicBackend] supports unordered streams too,
    /// then [`decode()`][Decoder::decode] will be populated with [`Chunk::Unordered`].
    ///
    /// **Note**: for most cases an application wants an ordered stream,
    /// which will be decoded into ordered sequence of messages.
    ///
    /// Unordered stream is a feature when an application receives all stream frames immediately,
    /// even if there are previous frames that are missing due to packet loss.
    ///
    /// Application may use unordered streams if there is no reason to preserve the order,
    /// for example when writing to file ([`Chunk::Unordered`] contains offset too).
    #[supports(quinn)]
    fn supports_unordered() -> bool;

    /// Decodes the next batch of byte chunks.
    /// Available items are written into `out_items`.
    ///
    /// If `fin` is `true`, this indicates the successful end of the stream; no further data will arrive.
    /// It is expected, that method returns all pending items on `FIN`.
    ///
    /// **Note**: it is expected that `&mut in_batch` will become empty after this call.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Decoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Decoder`] will not be used again after cancellation.
    fn decode(
        &mut self,
        in_batch: &mut Vec<Chunk>,
        out_items: &mut Vec<Self::Item>,
        fin: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + SendOnMt;
}

/// [`Encoder`] is a stateful object responsible for a single outgoing QUIC stream.
/// Its primary goal is to encode meaningful entities of type [`Self::Item`] into a raw byte stream.
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations or serialization).
///
/// The library is optimized for both blocking and non-blocking implementations.
pub trait Encoder {
    /// The type of the entity to be encoded.
    type Item;

    /// The type of error in case of encoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error;

    /// Encodes the next payload into `out_batch`.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Encoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Encoder`] will not be used again after cancellation.
    fn encode(
        &mut self,
        in_payload: Payload<Self::Item>,
        out_batch: &mut Vec<Bytes>,
    ) -> impl Future<Output = Result<(), Self::Error>> + SendOnMt;
}
