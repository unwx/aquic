use crate::core::{QuicCommand, QuicResponse};
use crate::exec::SendOnMt;
use crate::stream::{Chunk, Priority, StreamId};
use crate::sync::rpc;
use crate::sync::rpc::SendError;
use bytes::Bytes;
use smallvec::SmallVec;
use std::borrow::Cow;

#[derive(Debug, Clone)]
pub(crate) enum StreamError {
    /// Stream is finished.
    Finish,

    /// Stream is terminated with `STOP_SENDING` frame.
    StopSending(u64),

    /// Stream is terminated with `RESET_SENDING` frame.
    ResetSending(u64),

    /// Other stream error.
    Other(Cow<'static, str>),
}


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for incoming(read) stream direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub struct InStreamBackend<CId: SendOnMt + Unpin + 'static> {
    cid: CId,
    sid: StreamId,
    remote: rpc::Remote<QuicCommand<CId>, QuicResponse<CId>>,
}

impl<CId: Clone + SendOnMt + Unpin + 'static> InStreamBackend<CId> {
    pub fn new(
        cid: CId,
        sid: StreamId,
        remote: rpc::Remote<QuicCommand<CId>, QuicResponse<CId>>,
    ) -> Self {
        Self { cid, sid, remote }
    }

    /// [`QuicBackend::stream_recv`](crate::backend::QuicBackend::stream_recv).
    ///
    /// # Cancel Safety
    ///
    /// Not cancal safe, chunks will be lost forever.
    pub(crate) async fn recv(
        &mut self,
        max_total_size: usize,
    ) -> Result<Result<(SmallVec<[Chunk; 32]>, bool), StreamError>, SendError> {
        todo!();
    }

    /// Asynchronously sends 'STOP_SENDING': [`QuicBackend::stream_stop_sending`](crate::backend::QuicBackend::stream_stop_sending).
    pub fn stop_sending(&mut self, err: u64) {
        todo!();
    }
}


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for outgoing(write) stream direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub struct OutStreamBackend<CId: SendOnMt + Unpin + 'static> {
    cid: CId,
    sid: StreamId,
    remote: rpc::Remote<QuicCommand<CId>, QuicResponse<CId>>,
}

impl<CId: Clone + SendOnMt + Unpin + 'static> OutStreamBackend<CId> {
    pub fn new(
        cid: CId,
        sid: StreamId,
        remote: rpc::Remote<QuicCommand<CId>, QuicResponse<CId>>,
    ) -> Self {
        Self { cid, sid, remote }
    }


    /// Gives a batch of bytes, waiting until each of them is sent.
    ///
    /// **Note**: `sent` in this context **doesn't** mean that they're released,
    /// sent over the network or that peer received them.
    ///
    /// It simply means that `QuicBackend` approved that stream has capacity to send the rest.
    ///
    /// [`QuicBackend::stream_send`](crate::backend::QuicBackend::stream_send).
    ///
    /// # Cancel Safety
    ///
    /// Not cancel safe, partial write may happen.
    pub async fn send(
        &mut self,
        batch: SmallVec<[Bytes; 32]>,
        fin: bool,
    ) -> Result<Result<(), StreamError>, SendError> {
        todo!();
    }

    /// Asynchronously sets stream priority: [`QuicBackend::stream_set_priority`](crate::backend::QuicBackend::stream_set_priority).
    pub fn set_priority(&mut self, priority: Priority) {
        todo!();
    }

    /// Asynchronously sends 'RESET_STREAM': [`QuicBackend::stream_reset_sending`](crate::backend::QuicBackend::stream_reset_sending).
    pub fn reset_sending(&mut self, err: u64) {
        todo!();
    }
}
