use crate::backend::cid::{ConnectionId, ConnectionIdGenerator, IdError, MAX_CID_LEN};
use rand::{Rng, rng};

/// A simple [ConnectionIdGenerator] implementation,
/// generates purely random connection IDs.
pub struct RandomIdGenerator {
    length: usize,
}

impl RandomIdGenerator {
    /// Creates a new instance with a specified Connection ID length,
    /// which may be zero.
    ///
    /// # Panics
    ///
    /// If `length` is greater than [MAX_CID_LEN].
    pub fn new(length: usize) -> Self {
        assert!(
            length <= MAX_CID_LEN,
            "'length'({length}) must be <= {MAX_CID_LEN}"
        );

        Self { length }
    }
}

impl ConnectionIdGenerator for RandomIdGenerator {
    type Meta = ();

    fn generate(&self) -> ConnectionId {
        let mut cid = [0u8; MAX_CID_LEN];
        rng().fill_bytes(&mut cid[..self.length]);

        ConnectionId::try_from_slice(&cid[..self.length])
            .expect("'cid' length is guaranteed to be <= MAX_CID_LEN")
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
        Ok(())
    }
}
