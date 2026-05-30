use crate::{
    Spec,
    backend::{Error, StableConnectionId},
    core::{AquicCommand, AquicResponse},
    sync::mpsc::rpc::{self, SendError},
};
use std::marker::PhantomData;


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for incoming(read) datagram direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub(crate) struct InDgramBackend<S: Spec, CId: StableConnectionId> {
    connection_id: CId,
    remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    _phantom: PhantomData<S>,
}

impl<S: Spec, CId: StableConnectionId> InDgramBackend<S, CId> {
    pub fn new(connection_id: CId, remote: rpc::Remote<AquicCommand<CId>, AquicResponse>) -> Self {
        Self {
            connection_id,
            remote,
            _phantom: PhantomData,
        }
    }

    pub async fn recv(
        &mut self,
        decoder: S::DgramDecoder,
    ) -> Result<Result<S::DgramDecoder, Error>, SendError> {
        todo!();
    }
}


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for outgoing(write) datagram direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub(crate) struct OutDgramBackend<S: Spec, CId: StableConnectionId> {
    connection_id: CId,
    remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    _phantom: PhantomData<S>,
}

impl<S: Spec, CId: StableConnectionId> OutDgramBackend<S, CId> {
    pub fn new(connection_id: CId, remote: rpc::Remote<AquicCommand<CId>, AquicResponse>) -> Self {
        Self {
            connection_id,
            remote,
            _phantom: PhantomData,
        }
    }

    pub async fn send(
        &mut self,
        encoder: S::DgramEncoder,
    ) -> Result<Result<S::DgramEncoder, Error>, SendError> {
        todo!();
    }
}
