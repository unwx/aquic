use crate::{
    backend::cid::{ConnectionId, ConnectionIdGenerator},
    net::Ecn,
};
use quinn_proto::{EcnCodepoint, InvalidCid, VarInt};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};


impl From<Ecn> for Option<EcnCodepoint> {
    fn from(value: Ecn) -> Self {
        match value {
            Ecn::NEct => None,
            Ecn::Ect1 => Some(EcnCodepoint::Ect1),
            Ecn::Ect0 => Some(EcnCodepoint::Ect0),
            Ecn::Ce => Some(EcnCodepoint::Ce),
        }
    }
}

impl From<Option<EcnCodepoint>> for Ecn {
    fn from(value: Option<EcnCodepoint>) -> Self {
        match value {
            Some(ecn) => match ecn {
                EcnCodepoint::Ect0 => Ecn::Ect0,
                EcnCodepoint::Ect1 => Ecn::Ect1,
                EcnCodepoint::Ce => Ecn::Ce,
            },
            None => Ecn::NEct,
        }
    }
}

pub(crate) fn u64_into_varint(value: u64) -> VarInt {
    VarInt::from_u64(value).unwrap_or_else(|e| {
        panic!("unable to convert `u64` ({value}) into quinn_proto::VarInt: {e}");
    })
}


// `quinn_proto::ConnectionIdGenerator` has a `Sync` requirement.
//
// It is expected to have zero contention,
// but I also don't want to risk and implement unsafe `Sync` for it,
// because things change from time to time.
pub(super) struct SyncConnectionIdGenerator<T> {
    inner: Arc<Mutex<T>>,
    length: usize,
    lifetime: Option<Duration>,
}

impl<T: ConnectionIdGenerator> SyncConnectionIdGenerator<T> {
    pub fn new(inner: T) -> Self {
        let length = inner.cid_len();
        let lifetime = inner.cid_lifetime();

        Self {
            inner: Arc::new(Mutex::new(inner)),
            length,
            lifetime,
        }
    }
}

impl<T: ConnectionIdGenerator> quinn_proto::ConnectionIdGenerator for SyncConnectionIdGenerator<T> {
    fn generate_cid(&mut self) -> quinn_proto::ConnectionId {
        let cid = self.inner.lock().unwrap().generate();
        quinn_proto::ConnectionId::new(cid.as_slice())
    }

    fn validate(&self, cid: &quinn_proto::ConnectionId) -> Result<(), InvalidCid> {
        let mut cid = ConnectionId::from_slice(cid.as_ref());
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

impl<T> Clone for SyncConnectionIdGenerator<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            length: self.length,
            lifetime: self.lifetime,
        }
    }
}
