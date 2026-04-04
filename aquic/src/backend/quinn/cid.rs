use crate::backend::{ConnectionId, ConnectionIdGenerator};
use quinn_proto::InvalidCid;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// Aquic [ConnectionIdGenerator] adapter for Quinn [quinn_proto::ConnectionIdGenerator].
pub struct ConnectionIdGeneratorAdapter<T> {
    inner: Arc<Mutex<T>>,
    length: usize,
    lifetime: Option<Duration>,
}

impl<T: ConnectionIdGenerator> ConnectionIdGeneratorAdapter<T> {
    pub fn new(inner: T, lifetime: Option<Duration>) -> Self {
        let length = inner.cid_len();

        Self {
            inner: Arc::new(Mutex::new(inner)),
            length,
            lifetime,
        }
    }
}

impl<T: ConnectionIdGenerator> quinn_proto::ConnectionIdGenerator
    for ConnectionIdGeneratorAdapter<T>
{
    fn generate_cid(&mut self) -> quinn_proto::ConnectionId {
        let cid = self.inner.lock().unwrap().generate();
        quinn_proto::ConnectionId::new(cid.as_slice())
    }

    fn validate(&self, cid: &quinn_proto::ConnectionId) -> Result<(), InvalidCid> {
        let Some(mut cid) = ConnectionId::try_from_slice(cid.as_ref()) else {
            return Err(InvalidCid);
        };

        let inner = self.inner.lock().unwrap();

        if inner.validate(&cid) {
            return Ok(());
        }
        if inner.decrypt(&mut cid).is_err() {
            return Err(InvalidCid);
        };

        if inner.validate(&cid) {
            Ok(())
        } else {
            Err(InvalidCid)
        }
    }

    fn cid_len(&self) -> usize {
        self.length
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        self.lifetime
    }
}

impl<T> Clone for ConnectionIdGeneratorAdapter<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            length: self.length,
            lifetime: self.lifetime,
        }
    }
}
