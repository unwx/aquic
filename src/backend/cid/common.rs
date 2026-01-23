use crate::backend::NoopConnIdMeta;
use crate::backend::cid::{ConnIdError, ConnIdGenerator, ConnectionId};
use crate::util::ArrayVec;
use rand::{RngCore, rng};
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

impl ConnIdGenerator for RandConnIdGenerator {
    type Meta = NoopConnIdMeta;

    fn generate_cid(&mut self) -> ConnectionId {
        let mut buf = ArrayVec::zeroed();
        rng().fill_bytes(&mut buf.as_slice_all_mut()[..self.length]);

        ConnectionId(buf)
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

    fn decrypt(&self, _cid: &mut ConnectionId) -> Result<(), ConnIdError> {
        Ok(())
    }

    fn parse(&self, _cid: &ConnectionId) -> Result<Self::Meta, ConnIdError> {
        Ok(NoopConnIdMeta)
    }
}
