use crate::Spec;
use std::{
    borrow::Cow,
    fmt::{self, Debug, Display, Formatter},
};

/// Represents the terminal, final state of a stream **direction**.
pub enum Error<S: Spec> {
    /// The stream direction completed successfully.
    ///
    /// The sending-side finished sending data (`FIN`).
    Finish,

    /// The stream direction was abruptly terminated by the receiving-side (`STOP_SENDING`).
    Stop(S::StreamCode),

    /// The stream direction was abruptly terminated by the sending-side (`RESET_STREAM`).
    Reset(S::StreamCode),

    /// Deserialization of the incoming message failed.
    Decoder(S::StreamDecoderError),

    /// Serialization of the outgoing message failed.
    Encoder(S::StreamEncoderError),

    /// A QUIC connection, this stream belongs to, is closed.
    Connection,

    /// The sending/receiving side unexpectedly disappeared.
    HangUp(Cow<'static, str>),
}

impl<S: Spec> Clone for Error<S> {
    fn clone(&self) -> Self {
        match self {
            Self::Finish => Self::Finish,
            Self::Stop(e) => Self::Stop(*e),
            Self::Reset(e) => Self::Reset(*e),
            Self::Decoder(e) => Self::Decoder(e.clone()),
            Self::Encoder(e) => Self::Encoder(e.clone()),
            Self::Connection => Self::Connection,
            Self::HangUp(e) => Self::HangUp(e.clone()),
        }
    }
}

impl<S: Spec> Debug for Error<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finish => write!(f, "Finish"),
            Self::Stop(e) => f.debug_tuple("Stop").field(e).finish(),
            Self::Reset(e) => f.debug_tuple("Reset").field(e).finish(),
            Self::Decoder(e) => f.debug_tuple("Decoder").field(e).finish(),
            Self::Encoder(e) => f.debug_tuple("Encoder").field(e).finish(),
            Self::Connection => f.debug_tuple("Connection").finish(),
            Self::HangUp(e) => f.debug_tuple("HangUp").field(e).finish(),
        }
    }
}

impl<S: Spec> Display for Error<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finish => write!(f, "finish"),
            Self::Stop(e) => write!(f, "stop ({e})"),
            Self::Reset(e) => write!(f, "reset ({e})"),
            Self::Decoder(e) => write!(f, "decoder error ({e})"),
            Self::Encoder(e) => write!(f, "encoder error ({e})"),
            Self::Connection => write!(f, "connection is closed"),
            Self::HangUp(e) => write!(f, "hang-up ({e})"),
        }
    }
}

impl<S: Spec> std::error::Error for Error<S> {}
