use crate::backend::StreamResult;
use crate::backend::{StreamTask, StreamTaskResult};
use crate::stream::{Priority, StreamId};
use crate::sync::mpsc;
use crate::sync::mpsc::unbounded::{UnboundedReceiver, UnboundedSender};
use bytes::{Bytes, BytesMut};
use tracing::debug;

/// Provides [StreamBackend] backend API,
/// required for incoming(read) stream direction.
///
/// All the communication with [StreamBackend] is done using channels.
/// It sends a [StreamTask] and waits for [StreamResult] when necessary.
///
/// [StreamBackend]: crate::backend::StreamBackend
pub(crate) struct InStreamBackend {
    stream_id: StreamId,
    task_sender: mpsc::unbounded::Sender<(StreamId, StreamTask)>,
    result_receiver: mpsc::unbounded::Receiver<StreamTaskResult>,
}

impl InStreamBackend {
    pub fn new(
        stream_id: StreamId,
        task_sender: mpsc::unbounded::Sender<(StreamId, StreamTask)>,
        result_receiver: mpsc::unbounded::Receiver<StreamTaskResult>,
    ) -> Self {
        Self {
            stream_id,
            task_sender,
            result_receiver,
        }
    }

    /// [StreamBackend::stream_recv].
    ///
    /// Returns `None` if [StreamBackend] is unavailable.
    ///
    /// # Cancel Safety
    ///
    /// Not cancel safe.
    /// A next call might return old result, or panic.
    ///
    /// [StreamBackend]: crate::backend::StreamBackend
    /// [StreamBackend::stream_recv]: crate::backend::StreamBackend::stream_recv
    pub async fn recv(
        &mut self,
        buffer: BytesMut,
    ) -> Option<StreamResult<(BytesMut, usize, bool)>> {
        self.task_sender
            .send((self.stream_id, StreamTask::Recv(buffer)))
            .ok()?;

        match self.result_receiver.recv().await? {
            StreamTaskResult::Recv(it) => Some(it),
            other => panic!(
                "bug: received unexpected StreamTask result: expected 'Recv', but was '{}'",
                other.name()
            ),
        }
    }

    /// Asynchronously sends 'STOP_SENDING': [StreamBackend::stream_stop_sending].
    ///
    /// [StreamBackend::stream_stop_sending]: crate::backend::StreamBackend::stream_stop_sending
    pub fn stop_sending(&mut self, err: u64) {
        if let Err(e) = self
            .task_sender
            .send((self.stream_id, StreamTask::StopSending(err)))
        {
            debug!("failed to send 'STOP_SENDING({err})' to peer: {e}");
        }
    }
}


/// Provides [StreamBackend] backend API,
/// required for outgoing(write) stream direction.
///
/// All the communication with [StreamBackend] is done using channels.
/// It sends a [StreamTask] and waits for [StreamResult] when necessary.
///
/// It sends a [StreamTask] and waits for [StreamResult].
///
/// [StreamBackend]: crate::backend::StreamBackend
pub(crate) struct OutStreamBackend {
    stream_id: StreamId,
    task_sender: mpsc::unbounded::Sender<(StreamId, StreamTask)>,
    result_receiver: mpsc::unbounded::Receiver<StreamTaskResult>,
}

impl OutStreamBackend {
    pub fn new(
        stream_id: StreamId,
        task_sender: mpsc::unbounded::Sender<(StreamId, StreamTask)>,
        result_receiver: mpsc::unbounded::Receiver<StreamTaskResult>,
    ) -> Self {
        Self {
            stream_id,
            task_sender,
            result_receiver,
        }
    }

    /// [StreamBackend::stream_send].
    ///
    /// Returns `None` if [StreamBackend] is unavailable.
    ///
    /// # Cancel Safety
    ///
    /// Not cancel safe.
    /// A next call might return old result, or panic.
    ///
    /// [StreamBackend]: crate::backend::StreamBackend
    /// [StreamBackend::stream_send]: crate::backend::StreamBackend::stream_send
    pub async fn send(&mut self, bytes: Bytes, fin: bool) -> Option<StreamResult<Option<Bytes>>> {
        self.task_sender
            .send((self.stream_id, StreamTask::Send(bytes, fin)))
            .ok()?;

        match self.result_receiver.recv().await? {
            StreamTaskResult::Send(it) => Some(it),
            other => panic!(
                "bug: received unexpected StreamTask result: expected 'Send', but was '{}'",
                other.name()
            ),
        }
    }

    /// Asynchronously sets stream priority: [StreamBackend::stream_priority].
    ///
    /// [StreamBackend::stream_priority]: crate::backend::StreamBackend::stream_priority
    pub fn set_priority(&mut self, priority: Priority) {
        if let Err(e) = self
            .task_sender
            .send((self.stream_id, StreamTask::Priority(priority)))
        {
            debug!("failed to set stream priority: {e}");
        }
    }

    /// Asynchronously sends 'RESET_STREAM': [StreamBackend::stream_reset_sending].
    ///
    /// [StreamBackend::stream_reset_sending]: crate::backend::StreamBackend::stream_reset_sending
    pub fn reset_sending(&mut self, err: u64) {
        if let Err(e) = self
            .task_sender
            .send((self.stream_id, StreamTask::ResetSending(err)))
        {
            debug!("failed to send 'RESET_STREAM({err})' to peer: {e}");
        }
    }
}
