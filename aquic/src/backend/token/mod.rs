use crate::backend::ConnectionId;
use core::fmt;
use std::{
    fmt::{Display, Formatter},
    net::IpAddr,
    time::{SystemTime, UNIX_EPOCH},
};

mod padded;
pub use padded::*;

mod noop;
pub use noop::*;


/// A QUIC tokens generator, that is able to generate and verify:
/// - `Initial` tokens.
/// - `Stateless Reset` tokens.
pub trait TokenGenerator {
    /// Generates a unique, random `Retry` token.
    ///
    /// This token is going to be used to craft a `Retry` packet with it,
    /// and also to verify peer's identity receiving this token back.
    ///
    /// Generated token must follow the [RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000) requirements.
    ///
    /// Returns `None` if any of the provided connection IDs is longer than [MAX_CID_LEN].
    fn generate_retry_token(
        &mut self,
        peer_addr: IpAddr,
        peer_scid: &[u8],
        peer_dcid: &[u8],
        server_new_scid: &[u8],
        expires_at: SystemTime,
    ) -> Option<&[u8]>;

    /// Generates a unique, random Identity token.
    ///
    /// This token is going to be used to craft a `NEW_TOKEN` frame with it,
    /// and also to verify peer's identity receiving this token back.
    ///
    /// Generated token must follow the [RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000) requirements.
    fn generate_identity_token(&mut self, peer_addr: IpAddr, expires_at: SystemTime) -> &[u8];

    /// Generates a [Stateless Reset](https://datatracker.ietf.org/doc/html/rfc9000#stateless-reset).
    ///
    /// Generated token must follow the [RFC 9000](https://datatracker.ietf.org/doc/html/rfc9000) requirements.
    fn generate_reset_token(&mut self, server_scid: &[u8]) -> u128;


    /// Verifies a token that was attached to an `Initial` packet.
    ///
    /// It may be one of the `Retry` or `Identity` tokens.
    fn verify_initial_token(
        &mut self,
        peer_addr: IpAddr,
        peer_scid: &[u8],
        peer_dcid: &[u8],
        token: &[u8],
        now: SystemTime,
    ) -> Result<Token, TokenError>;

    /// Verifies the provided reset `token` against `server_scid`.
    fn verify_reset_token(&mut self, server_scid: &[u8], token: u128) -> Result<(), TokenError>;
}


/// `Initial` packet token kind.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// Single-use `Retry` token.
    Retry,

    /// General use token for peer's identity verification.
    Identity,
}

impl Display for TokenKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Retry => write!(f, "retry token"),
            TokenKind::Identity => write!(f, "identity token"),
        }
    }
}


/// An `Initial` token verification error.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Provided token is invalid, and may be malicious.
    Invalid,

    /// Provided token is valid, but expired.
    ///
    /// Contains token kind and expiration time.
    Expired(TokenKind, SystemTime),
}

impl Display for TokenError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::Invalid => write!(f, "token is invalid"),
            TokenError::Expired(token_kind, expired_at) => {
                let expired_at = expired_at.duration_since(UNIX_EPOCH).unwrap_or_default();

                write!(
                    f,
                    "token {} was expired at {}",
                    token_kind,
                    expired_at.as_millis()
                )
            }
        }
    }
}


/// A decrypted and verified `Initial` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Retry token.
    Retry {
        peer_original_dcid: ConnectionId,
        expires_at: SystemTime,
    },

    /// Token, that client might use to start a new QUIC connection
    /// without verifying his identity (source address) with `Retry` again.
    ///
    /// This token **may** be [reused multiple times](https://datatracker.ietf.org/doc/html/rfc9000#section-8.1.3-8).
    ///
    /// **Security Note**: it doesn't provide 100% guarantee of peer's identity.
    Identity { expires_at: SystemTime },
}

impl Token {
    /// Returns token's expiration time.
    pub fn expires_at(&self) -> SystemTime {
        match self {
            Token::Retry { expires_at, .. } | Token::Identity { expires_at } => *expires_at,
        }
    }
}
