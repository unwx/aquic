use crate::backend::cid::{ConnectionId, ConnectionIdGenerator, IdError, NoopIdMeta};
use rand::{RngCore, rng};
use smallvec::SmallVec;

/// A [ConnIdGenerator] implementation that
/// simply generates random [ConnectionId] without any additional information.
pub struct RandConnIdGenerator {
    length: usize,
}

impl RandConnIdGenerator {
    /// Create a new instance with a specified Connection ID length in bytes.
    pub fn new(length: usize) -> Self {
        Self { length }
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
