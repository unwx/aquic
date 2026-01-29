use crate::exec::SendOnMt;
use crate::stream::{Chunk, Payload};
use aquic_macros::supports;
use bytes::Bytes;
use smallvec::SmallVec;
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
    type Item: SendOnMt + 'static;

    /// The type of error in case of decoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error + Clone + Send + Sync + 'static;

    /// Returns `true` if the decoder supports an unordered stream.
    ///
    /// If a current [`QUIC Backend`][crate::backend::QuicBackend] supports unordered streams too,
    /// then [`read()`][Decoder::read] will be populated with [`Chunk::Unordered`].
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

    /// Returns a maximum size for a **sum** of bytes
    /// that decoder wants to receive in a single [`read()`][Decoder::read] call.
    fn max_batch_size(&self) -> usize;

    /// Notifies about new available data for decoding.
    ///
    /// If `fin` is `true`, this indicates the successful end of the stream; no further data will arrive.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Decoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Decoder`] will not be used again after cancellation.
    fn read(
        &mut self,
        chunks: SmallVec<[Chunk; 32]>,
        fin: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + SendOnMt;

    /// Returns the next decoded item, or `None` if there is no item available.
    ///
    /// A specific item will not be returned twice (unless it was explicitly received twice).
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Decoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Decoder`] will not be used again after cancellation.
    fn next_item(
        &mut self,
    ) -> impl Future<Output = Result<Option<Self::Item>, Self::Error>> + SendOnMt;

    /// Returns `true` if decoder received `fin` previously in [`read()`][Decoder::read],
    /// and has no more data in [`next_item()`][Decoder::next_item].
    fn is_fin(&self) -> bool;
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
    type Item: SendOnMt + 'static;

    /// The type of error in case of encoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error + Clone + Send + Sync + 'static;

    /// Queues an item to be written to the stream.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Encoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Encoder`] will not be used again after cancellation.
    fn write(
        &mut self,
        payload: Payload<Self::Item>,
    ) -> impl Future<Output = Result<(), Self::Error>> + SendOnMt;

    /// Returns the next chunk of raw bytes to be sent to the peer,
    /// or empty if there is nothing to send.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Encoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Encoder`] will not be used again after cancellation.
    fn next_buffer(
        &mut self,
    ) -> impl Future<Output = Result<SmallVec<[Bytes; 32]>, Self::Error>> + SendOnMt;

    /// Returns `true` if encoder received `fin` previously in [`write()`][Encoder::write],
    /// and has no more data in [`next_buffer()`][Encoder::next_buffer].
    fn is_fin(&self) -> bool;
}
