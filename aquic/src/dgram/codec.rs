use bytes::Bytes;

/// A QUIC connection, as a part of its [extension](https://datatracker.ietf.org/doc/html/rfc9221),
/// allows sending and receiving unreliable datagrams.
///
/// `Decoder` is a stateful object created per connection.
/// Its primary goal is to decode raw unordered datagram into meaningful entities of type [`Self::Item`],
/// where "decode" may be (not limited to):
/// - Remove custom protocol formatting.
/// - Decompress.
///
/// `Decoder` is not meant to replace deserializers: by design, they should work together (`... -> decoder -> application-deserializer`).
/// Though it's up to application to decide how to use it.
pub trait Decoder {
    /// The type of the decoded entity.
    type Item;

    /// Reads batch of datagrams `&mut in_batch`, decodes them and writes the result into `&mut out_batch`.
    ///
    /// It is up to application to design how to handle decoding errors, you can:
    /// - Just log them.
    /// - Make [`Self::Item`] a `Result<T, E>`
    /// - etc.
    fn decode(&mut self, in_batch: &mut Vec<Bytes>, out_batch: &mut Vec<Self::Item>);
}

/// A QUIC connection, as a part of its [extension](https://datatracker.ietf.org/doc/html/rfc9221),
/// allows sending and receiving unreliable datagrams.
///
/// `Encoder` is a stateful object created per connection.
/// Its primary goal is to encode entities of type [`Self::Item`] into raw datagrams,
/// where "encode" may be (not limited to):
/// - Apply custom protocol formatting.
/// - Compress.
///
/// `Encoder` is not meant to replace serializers: by design, they should work together (`... -> application-serializer` -> `encoder`).
/// Though it's up to application to decide how to use it.
pub trait Encoder {
    /// The type of the entity to be encoded.
    type Item;

    /// Encodes the `item` into `&mut out_batch`.
    fn encode(&mut self, item: Self::Item, out_batch: &mut Vec<Bytes>);
}
