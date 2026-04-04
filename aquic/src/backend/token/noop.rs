use crate::backend::{Token, TokenError, TokenGenerator};
use std::{net::IpAddr, time::SystemTime};

/// A token generator that panics on all operations.
#[derive(Debug, Copy, Clone)]
pub struct PanicTokenGenerator(pub &'static str);

impl TokenGenerator for PanicTokenGenerator {
    fn generate_retry_token(
        &mut self,
        _: IpAddr,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: SystemTime,
    ) -> Option<&[u8]> {
        panic!("{}", self.0);
    }

    fn generate_identity_token(&mut self, _: IpAddr, _: SystemTime) -> &[u8] {
        panic!("{}", self.0);
    }

    fn generate_reset_token(&mut self, _: &[u8]) -> u128 {
        panic!("{}", self.0);
    }

    fn verify_initial_token(
        &mut self,
        _: IpAddr,
        _: &[u8],
        _: &[u8],
        _: &[u8],
        _: SystemTime,
    ) -> Result<Token, TokenError> {
        panic!("{}", self.0);
    }

    fn verify_reset_token(&mut self, _: &[u8], _: u128) -> Result<(), TokenError> {
        panic!("{}", self.0);
    }
}
