use crate::backend::cid::{ConnectionId, ConnectionIdGenerator, IdError, NoopIdMeta};
use rand::{RngCore, rng};
use smallvec::SmallVec;
use std::time::Duration;

/// A [ConnIdGenerator] implementation that
/// simply generates random [ConnectionId] without any additional information.
pub struct RandConnIdGenerator {
    length: usize,
    lifetime: Option<Duration>,
}

impl RandConnIdGenerator {
    /// Create a new instance with a specified Connection ID length in bytes.
    pub fn new(length: usize) -> Self {
        Self {
            length,
            lifetime: None,
        }
    }

    /// Create a new instance with:
    /// * `length` a Connection ID length in bytes.
    /// * `lifetime` a Connection ID lifetime.
    pub fn new_with_lifetime(length: usize, lifetime: Duration) -> Self {
        Self {
            length,
            lifetime: Some(lifetime),
        }
    }
}

impl ConnectionIdGenerator for RandConnIdGenerator {
    type Meta = NoopIdMeta;

    fn generate(&mut self) -> ConnectionId {
        let mut buffer = SmallVec::from_elem(0, self.length);
        rng().fill_bytes(&mut buffer.as_mut_slice()[..self.length]);

        ConnectionId(buffer)
    }

    fn cid_len(&self) -> usize {
        self.length
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        self.lifetime
    }

    fn validate(&self, cid: &ConnectionId) -> bool {
        self.length == cid.len()
    }

    fn decrypt(&self, _cid: &mut ConnectionId) -> Result<(), IdError> {
        Ok(())
    }

    fn parse(&self, _cid: &ConnectionId) -> Result<Self::Meta, IdError> {
        Ok(NoopIdMeta)
    }
}
