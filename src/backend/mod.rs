mod stream;

pub(crate) use stream::*;


use crate::conditional;

conditional! {
    feature = "quiche",

    mod quiche;
    use crate::backend::quiche::Quiche;

    /// A [Backend] implementation.
    ///
    /// [Cloudflare/quiche](https://github.com/cloudflare/quiche) is used as an implementation of the QUIC protocol.
    #[cfg(feature = "quiche")]
    pub(crate) type QuicBackend = Quiche;
}


/// An API for various implementations of the QUIC protocol.
///
/// **Note**: protocol does not work with sockets.
/// It has its own state, buffers.
/// Data is received/sent from/over the socket in the networking part of this library.
pub(crate) trait Backend: StreamBackend {
    // TODO: packet methods, like send/recv.
    // TODO: congestion control methods, pacing methods.
}
