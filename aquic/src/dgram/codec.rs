use bytes::Bytes;

/// A QUIC connection, as a part of its [extension](https://datatracker.ietf.org/doc/html/rfc9221),
/// allows sending and receiving unreliable datagrams.
///
/// `Decoder` is an object created per connection.
/// Its primary goal is to decode raw datagrams into meaningful entities of type [`Self::Item`].
pub trait Decoder {
    /// The type of the decoded entity.
    type Item;

    /// Received a raw datagram.
    ///
    /// This method **must not** perform any expensive operation,
    /// as it may be invoked from a network I/O loop thread.
    ///
    /// Example of a permitted operation:
    /// - `memcpy/copy_from_slice` to a preallocated slice.
    /// - memory allocation (on your own risk).
    /// - fast deserialization/decoding algorithms, like [`rkyv`](https://github.com/rkyv/rkyv).
    ///
    /// Example of expensive operation:
    /// - deserialize json.
    fn recv(&mut self, bytes: &[u8]);

    /// Returns a decoded item, or `None` if no items left.
    fn decode(&mut self) -> Option<Self::Item>;
}

/// A QUIC connection, as a part of its [extension](https://datatracker.ietf.org/doc/html/rfc9221),
/// allows sending and receiving unreliable datagrams.
///
/// `Encoder` is an object created per connection.
/// Its primary goal is to encode entities of type [`Self::Item`] into raw datagrams.
pub trait Encoder {
    /// The type of the entity to be encoded.
    type Item;

    /// Receives and encodes an item.
    fn encode(&mut self, item: Self::Item);

    /// Returns `true` if all the previously encoded items
    /// should be [`flushed`](Encoder::flush) immediately.
    fn should_flush(&self) -> bool;

    /// Returns a next datagram to send.
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
    fn flush(&mut self) -> Option<Bytes>;
}
