use bytes::Bytes;

use crate::{
    backend::Error,
    core::{QuicCommand, QuicResponse},
    exec::SendOnMt,
    sync::mpsc::rpc::{self, SendError},
};


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for outgoing(write) datagram direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub(crate) struct OutDgramBackend<CId: SendOnMt + Unpin + 'static> {
    connection_id: CId,
    remote: rpc::Remote<QuicCommand<CId>, QuicResponse>,
}

impl<CId: Clone + SendOnMt + Unpin + 'static> OutDgramBackend<CId> {
    pub fn new(connection_id: CId, remote: rpc::Remote<QuicCommand<CId>, QuicResponse>) -> Self {
        Self {
            connection_id,
            remote,
        }
    }

    pub async fn send(
        &mut self,
        batch: Vec<Bytes>,
    ) -> Result<Result<Vec<Bytes>, Error>, SendError> {
        todo!();
    }

    pub fn dgram_set_drop_unsent(&mut self) {
        todo!();
    }
}
