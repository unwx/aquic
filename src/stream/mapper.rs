use bytes::Bytes;


/// A factory for creating new instances of [`StreamReader`].
pub trait StreamReaderFactory<T, E, SR>
where
    SR: StreamReader<T, E>,
{
    /// Creates a new [`StreamReader`].
    fn new_stream_reader(&self) -> SR;
}

/// A factory for creating new instances of [`StreamWriter`].
pub trait StreamWriterFactory<T, E, SW>
where
    SW: StreamWriter<T, E>,
{
    /// Creates a new [`StreamWriter`].
    fn new_stream_writer(&self) -> SW;
}


/// A QUIC connection consists of multiple streams where data flows.
///
/// [`StreamReader`] is a stateful object responsible for a single incoming QUIC stream.
/// Its primary goal is to map the raw byte stream into meaningful messages of type [`T`].
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations).
///
/// The library is optimized for both blocking and non-blocking implementations.
pub trait StreamReader<T, E> {
    /// Returns a buffer to store incoming raw data.
    ///
    /// The buffer may be uninitialized (e.g., not filled with zeros).
    ///
    /// - Only one buffer may exist at a time.
    /// - The returned slice is **never empty**.
    fn buffer(&mut self) -> &mut [u8];

    /// Notifies the reader that data was written into [`StreamReader::buffer()`].
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
    /// If the future is dropped before completion, the [`StreamReader`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`StreamReader`] will not be used again after cancellation.
    fn notify_read(
        &mut self,
        length: usize,
        finish: bool,
    ) -> impl Future<Output = Result<(), E>> + Send;

    /// Returns `true` if the [`StreamReader`] has a message available
    /// and is ready to yield it via [`StreamReader::next_message()`].
    fn has_message(&self) -> bool;

    /// Returns the next mapped message, or `None` if [`StreamReader::has_message`] is false.
    ///
    /// A specific message will not be returned twice (unless it was explicitly received twice).
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`StreamReader`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`StreamReader`] will not be used again after cancellation.
    fn next_message(&mut self) -> impl Future<Output = Result<Option<T>, E>> + Send;
}

/// [`StreamWriter`] is a stateful object responsible for a single outgoing QUIC stream.
/// Its primary goal is to map meaningful messages of type [`T`] into a raw byte stream.
///
/// # Non-blocking implementation
///
/// While methods return `Future`s, a non-blocking implementation is not mandatory.
/// Implementations should only be non-blocking if necessary (e.g., when performing
/// expensive I/O operations or serialization).
///
/// The library is optimized for both blocking and non-blocking implementations.
pub trait StreamWriter<T, E> {
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
    /// If the future is dropped before completion, the [`StreamWriter`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`StreamWriter`] will not be used again after cancellation.
    fn write(
        &mut self,
        message: Option<T>,
        finish: bool,
    ) -> impl Future<Output = Result<(), E>> + Send;

    /// Returns `true` if the [`StreamWriter`] has raw data available
    /// and is ready to yield it via [`StreamWriter::next_buffer()`].
    fn has_buffer(&self) -> bool;

    /// Returns the next chunk of raw bytes to be sent to the peer,
    /// or [`Bytes::new`] if [`StreamWriter::has_buffer`] is `false`.
    ///
    /// # Cancel Safety
    ///
    /// This method is **not required** to be cancel-safe.
    ///
    /// If the future is dropped before completion, the [`StreamWriter`] may be left
    /// in an inconsistent or invalid state. However, it is guaranteed that the
    /// [`StreamWriter`] will not be used again after cancellation.
    fn next_buffer(&mut self) -> impl Future<Output = Result<Bytes, E>> + Send;
}
