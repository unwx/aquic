use crate::stream::Payload;
use bytes::Bytes;

/// A QUIC connection consists of multiple streams where data flows.
///
/// [`Decoder`] is a stateful object responsible for a single incoming QUIC stream.
/// Its primary goal is to decode the protocol-formatted raw byte stream into meaningful entities of type [`Self::Item`].
/// It may also uncompress the traffic, etc.
///
/// **Note**: it doesn't meant to replace deserializers, though it may in some cases.
pub trait Decoder {
    /// The type of the decoded entity.
    type Item;

    /// The type of error in case of decoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error;

    /// Returns a mutable buffer,
    /// that is going to be used to receive incoming stream data.
    ///
    /// This method paired with [`notify()`](Decoder::notify) are going to be invoked until `None` is received.
    ///
    /// The `mandatory` parameter specifies,
    /// whether it is **mandatory** to return `Some` with a non-empty buffer, or not.
    /// Usually, the first `recv()` call requires a mandatory buffer,
    /// while the further calls do not.
    ///
    /// # Warning
    ///
    /// This method **must not** perform any expensive operation,
    /// as it may be invoked from a network I/O loop thread.
    fn recv(&mut self, mandatory: bool) -> Option<&mut [u8]>;

    /// Notifies that previously returned [`recv()`][Decoder::recv] buffer
    /// was filled with `length` bytes of stream data.
    ///
    /// `FIN` flag indicates, whether it's a successful end of the stream, or not yet.
    ///
    /// # Warning
    ///
    /// This method **must not** perform any expensive operation,
    /// as it may be invoked from a network I/O loop thread.
    fn notify(&mut self, length: usize, fin: bool);

    /// Decodes previously received stream data,
    /// and returns a [Self::Item] if it's ready.
    ///
    /// This method is going to be invoked until `None`, `Some(FIN)` or `Err` is received.
    fn decode(&mut self) -> Result<Option<Payload<Self::Item>>, Self::Error>;
}

/// [`Encoder`] is a stateful object responsible for a single outgoing QUIC stream.
/// Its primary goal is to encode meaningful entities of type [`Self::Item`] into a protocol-formatted raw byte stream.
/// It may also compress the traffic, etc.
///
/// **Note**: it doesn't meant to replace serializers, though it may in some cases.
pub trait Encoder {
    /// The type of the entity to be encoded.
    type Item;

    /// The type of error in case of encoding failures.
    ///
    /// Clones should be cheap.
    type Error: std::error::Error;

    /// Receives and encodes a next payload.
    fn encode(&mut self, payload: Payload<Self::Item>) -> Result<(), Self::Error>;

    /// Returns `true` if all the previously encoded data
    /// should be [`flushed`](Encoder::flush) immediately.
    fn should_flush(&self) -> bool;

    /// Returns chunks of bytes with a `FIN` flag, which specifies,
    /// whether it's a successful end of the stream or not.
    ///
    /// This method is going to be invoked until `None` is received.
    ///
    /// # Bytes
    ///
    /// Little hint:
    ///
    /// If you use a custom bytes pool implementation,
    /// like thread local `VecDeque<[u8; 4096]>`,
    /// you can use [Bytes::from_owner] to free the borrowed bytes back to the pool.
    fn flush(&mut self) -> Option<(Bytes, bool)>;
}
