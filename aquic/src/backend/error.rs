use std::{
    borrow::Cow,
    fmt::{Display, Formatter},
};

/// [QuicBackend](crate::backend::QuicBackend) result.
pub type Result<T> = std::result::Result<T, Error>;

/// [QuicBackend](crate::backend::QuicBackend) error.
#[derive(Debug, Clone)]
pub enum Error {
    /// Backend is closed.
    Closed,

    /// Illegal operation or argument.
    ///
    /// For example:
    /// - Attempt to create a new client connection,
    ///   while backend is working on server-only mode.
    /// - Attempt to write data into read-only stream.
    Illegal,

    /// Provided buffer size is either too short or too long.
    BufferSize,

    /// Connection associated with the provided `connection_id` cannot be found.
    UnknownConnection,

    /// Stream associated with the provided `stream_id` cannot be found.
    UnknownStream,

    /// A refused attempt to open a new stream with a specific direction.
    ///
    /// The streams in the given direction are currently exhausted.
    StreamsExhausted,

    /// Unable to perform operation on the stream: it's terminated with `STOP_SENDING(err)`.
    StreamStop(u64),

    /// Unable to perform operation on the stream: it's terminated with `RESET_STREAM(err)`.
    StreamReset(u64),

    /// Datagrams are not supported by the current configuration, provider, or the peer.
    ///
    /// Note, that peer may have disabled sending datagrams for us,
    /// but still may send datagrams back to us.
    DgramDisabled,

    /// Other uncovered error.
    Other(Cow<'static, str>),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Closed => write!(f, "backend is closed"),
            Error::Illegal => write!(f, "illegal operation"),
            Error::BufferSize => write!(f, "provided buffer has invalid size"),
            Error::UnknownConnection => write!(f, "unknown connection"),
            Error::UnknownStream => write!(f, "unknown stream"),
            Error::StreamsExhausted => write!(f, "streams are exhausted on this direction"),
            Error::StreamStop(e) => {
                write!(f, "stream is terminated with 'STOP_SENDING({e})'")
            }
            Error::StreamReset(e) => {
                write!(f, "stream is terminated with 'RESET_STREAM({e})'")
            }
            Error::DgramDisabled => {
                write!(
                    f,
                    "datagrams are not supported on this direction, or completely"
                )
            }
            Error::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}
