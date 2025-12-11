use bytes::Bytes;


/// A factory for creating new instances of [`Decoder`].
pub trait DecoderFactory<T, E, SD>
where
    SD: Decoder<T, E>,
{
    /// Creates a new [`Decoder`].
    fn new_decoder(&self) -> SD;
}

/// A factory for creating new instances of [`Encoder`].
pub trait EncoderFactory<T, E, SE>
where
    SE: Encoder<T, E>,
{
    /// Creates a new [`Encoder`].
    fn new_encoder(&self) -> SE;
}


/// A QUIC connection consists of multiple streams where data flows.
///
/// [`Decoder`] is a stateful object responsible for a single incoming QUIC stream.
/// Its primary goal is to decode the raw byte stream into meaningful messages of type [`T`].
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations).
///
/// The library is optimized for both blocking and non-blocking implementations.
pub trait Decoder<T, E> {
    /// Returns a buffer to store incoming raw data.
    ///
    /// The buffer may be uninitialized (e.g., not filled with zeros).
    ///
    /// - Only one buffer may exist at a time.
    /// - The returned slice is **never empty**.
    fn buffer(&mut self) -> &mut [u8];

    /// Notifies the decoder that data was written into [`Decoder::buffer()`].
    ///
    /// If an error occurs, `Err(E)` should be returned, which will be delivered to the peer.
    ///
    /// # Arguments
    ///
    /// - `length`: The number of bytes written.
    /// - `finish`: If `true`, indicates the successful end of the stream; no further data will arrive.
    ///
    /// `length` is never `0`, unless `finish` is `true`.
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
        length: usize,
        finish: bool,
    ) -> impl Future<Output = Result<(), E>> + Send;

    /// Returns `true` if the [`Decoder`] has a message available
    /// and is ready to yield it via [`Decoder::next_message()`].
    fn has_message(&self) -> bool;

    /// Returns the next mapped message, or `None` if [`Decoder::has_message`] is false.
    ///
    /// A specific message will not be returned twice (unless it was explicitly received twice).
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Decoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Decoder`] will not be used again after cancellation.
    fn next_message(&mut self) -> impl Future<Output = Result<Option<T>, E>> + Send;
}

/// [`Encoder`] is a stateful object responsible for a single outgoing QUIC stream.
/// Its primary goal is to encode meaningful messages of type [`T`] into a raw byte stream.
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations or serialization).
///
/// The library is optimized for both blocking and non-blocking implementations.
pub trait Encoder<T, E> {
    /// Queues a message to be written to the stream.
    ///
    /// # Arguments
    ///
    /// - `message`: The message to write. This can be `None` **only if** `finish` is `true`.
    /// - `finish`: If `true`, indicates the successful end of the stream; no further messages will be sent.
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
        message: Option<T>,
        finish: bool,
    ) -> impl Future<Output = Result<(), E>> + Send;

    /// Returns `true` if the [`Encoder`] has raw data available
    /// and is ready to yield it via [`Encoder::next_buffer()`].
    fn has_buffer(&self) -> bool;

    /// Returns the next chunk of raw bytes to be sent to the peer,
    /// or [`Bytes::new`] if [`Encoder::has_buffer`] is `false`.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`Encoder`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`Encoder`] will not be used again after cancellation.
    fn next_buffer(&mut self) -> impl Future<Output = Result<Bytes, E>> + Send;
}
