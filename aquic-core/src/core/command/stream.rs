use crate::Spec;
use crate::backend::{Error, StableConnectionId};
use crate::core::{AquicCommand, AquicResponse};
use crate::stream::{Priority, StreamId};
use crate::sync::mpsc::rpc::{self, SendError};
use std::borrow::Cow;
use std::marker::PhantomData;

/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for incoming(read) stream direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub(crate) struct InStreamBackend<S: Spec, CId: StableConnectionId> {
    cid: CId,
    sid: StreamId,
    remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    _phantom: PhantomData<S>,
}

impl<S: Spec, CId: StableConnectionId> InStreamBackend<S, CId> {
    pub fn new(
        cid: CId,
        sid: StreamId,
        remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    ) -> Self {
        Self {
            cid,
            sid,
            remote,
            _phantom: PhantomData,
        }
    }

    pub async fn recv(
        &mut self,
        decoder: S::StreamDecoder,
    ) -> Result<Result<S::StreamDecoder, Error>, SendError> {
        todo!();
    }

    pub fn terminate(&mut self, err: S::StreamCode) {
        todo!();
    }

    pub fn terminate_due_decoder(&mut self, err: S::StreamDecoderError) {
        todo!();
    }

    pub fn hangup(&mut self, err: Cow<'static, str>) {
        todo!();
    }
}


/// Provides [`QuicBackend`][crate::backend::QuicBackend] API,
/// limiting to methods required only for outgoing(write) stream direction.
///
/// All the communication with `QuicBackend` is done using channels, asynchronously.
pub(crate) struct OutStreamBackend<S: Spec, CId: StableConnectionId> {
    cid: CId,
    sid: StreamId,
    remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    _phantom: PhantomData<S>,
}

impl<S: Spec, CId: StableConnectionId> OutStreamBackend<S, CId> {
    pub fn new(
        cid: CId,
        sid: StreamId,
        remote: rpc::Remote<AquicCommand<CId>, AquicResponse>,
    ) -> Self {
        Self {
            cid,
            sid,
            remote,
            _phantom: PhantomData,
        }
    }

    pub async fn send(
        &mut self,
        encoder: S::StreamEncoder,
    ) -> Result<Result<S::StreamEncoder, Error>, SendError> {
        todo!();
    }

    pub fn set_priority(&mut self, priority: Priority) {
        todo!();
    }

    pub fn terminate(&mut self, err: S::StreamCode) {
        todo!();
    }

    pub fn terminate_due_encoder(&mut self, err: S::StreamEncoderError) {
        todo!();
    }

    pub fn hangup(&mut self, err: Cow<'static, str>) {
        todo!();
    }
}
