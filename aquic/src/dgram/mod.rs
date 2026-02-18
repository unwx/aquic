//! [An Unreliable Datagram Extension to QUIC](https://datatracker.ietf.org/doc/html/rfc9221)
use std::{
    borrow::Cow,
    fmt::{self, Display, Formatter},
};


mod codec;
mod incoming;
mod outgoing;

pub use codec::*;
pub(crate) use incoming::*;
pub(crate) use outgoing::*;


/// A datagram direction error.
#[derive(Debug, Clone)]
pub enum Error {
    /// Sending/receiving side no more interested in datagrams.
    End,

    /// A QUIC connection is closed.
    Connection,

    /// Internal error, unexpected issue.
    HangUp(Cow<'static, str>),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::End => write!(f, "normal end"),
            Error::Connection => write!(f, "connection is closed"),
            Error::HangUp(e) => write!(f, "hang-up ({e})"),
        }
    }
}

impl std::error::Error for Error {}
