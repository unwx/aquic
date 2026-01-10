use crate::executor::SendIfRt;
use crate::stream::Payload;
use bytes::{Bytes, BytesMut};
use std::future::Future;

/// A factory for creating new instances of [`Decoder`].
pub trait DecoderFactory {
    /// The concrete [`Decoder`] implementation created by this factory.
    type Decoder: Decoder;

    /// Creates a new [`Decoder`] instance.
    fn new_decoder() -> Self::Decoder;
}

/// A factory for creating new instances of [`Encoder`].
pub trait EncoderFactory {
    /// The concrete [`Encoder`] implementation created by this factory.
    type Encoder: Encoder;

    /// Creates a new [`Encoder`] instance.
    fn new_encoder() -> Self::Encoder;
}

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
pub trait Decoder {
    /// The type of the decoded entity.
    type Item: SendIfRt + 'static;

    /// The type of error in case of decoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error + Clone + Send + Sync + 'static;

    /// Returns a mutable buffer to store incoming raw data.
    ///
    /// It is guaranteed to not store more than [BytesMut::len] bytes.
    ///
    /// Must be not empty.
    fn buffer(&mut self) -> BytesMut;

    /// Notifies the decoder that data was written into the buffer returned by [`Decoder::buffer()`].
    ///
    /// # Arguments
    ///
    /// - `buffer` The buffer that was previously returned by [`Decoder::buffer`].
    /// - `length`: The number of bytes actually written to the buffer.
    /// - `finish`: If `true`, indicates the successful end of the stream; no further data will arrive.
    ///
    /// # Panics
    ///
    /// Implementations may panic if
    /// `length` exceeds the size of the slice previously returned by [`buffer()`](Self::buffer).
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Decoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Decoder`] will not be used again after cancellation.
    fn notify_read(
        &mut self,
        buffer: BytesMut,
        length: usize,
        finish: bool,
    ) -> impl Future<Output = Result<(), Self::Error>> + SendIfRt;

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
    ) -> impl Future<Output = Result<Option<Self::Item>, Self::Error>> + SendIfRt;
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
    type Item: SendIfRt + 'static;

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
    ) -> impl Future<Output = Result<(), Self::Error>> + SendIfRt;

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
    fn next_buffer(&mut self) -> impl Future<Output = Result<Bytes, Self::Error>> + SendIfRt;
}
