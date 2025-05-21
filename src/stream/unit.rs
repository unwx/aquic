use crate::ApplicationError;
use crate::future::poll;
use crate::future::state::{Observer, Switch};
use crate::stream::buffer::StreamBuffer;
use crate::stream::channel::{RecvError, SendError, StreamReceiver, StreamSender};
use crate::stream::mapper::{StreamReader, StreamWriter};
use bytes::Bytes;
use quiche::{BufFactory, BufSplit, Connection, Error, Shutdown};
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use std::task::Poll;
use tokio::select;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;
use tracing::{error, warn};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Ready {
    pub read_from_wire: bool,
    pub write_to_wire: bool,
}

impl Ready {
    pub fn new(read_from_wire: bool, write_to_wire: bool) -> Self {
        Self {
            read_from_wire,
            write_to_wire,
        }
    }
}


pub struct ReadyFuture<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    inner: FutureResult<T, E, SR, SW>,
}

impl<T, E, SR, SW> From<FutureResult<T, E, SR, SW>> for ReadyFuture<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    fn from(inner: FutureResult<T, E, SR, SW>) -> Self {
        Self { inner }
    }
}


pub struct Stream<T, E, SR, SW, BF>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
    BF: BufFactory,
{
    id: u64,
    open: Arc<AtomicBool>,

    in_state: Option<InState<T, E, SR>>,
    out_state: Option<OutState<T, E, SW>>,

    // Used only with 'out_state' present or in 'write_pending_bytes_to_wire()'.
    out_buffer: Arc<Mutex<StreamBuffer>>,

    futures: JoinSet<()>,
    in_interrupt_switch: Switch<DirectionError<E>>,
    out_interrupt_switch: Switch<DirectionError<E>>,
    ready_futures_sender: UnboundedSender<(u64, ReadyFuture<T, E, SR, SW>)>,

    // TODO(docs): Approximate.
    //  Buffer may use more than specified here,
    //  depends on SW result.
    out_max_buffer_length: usize,

    // A single stream cannot be used between different connections,
    // for simplicity quiche::BufFactory is declared here.
    _phantom: PhantomData<BF>,
}

impl<T, E, SR, SW, BF> Stream<T, E, SR, SW, BF>
where
    T: Send + 'static,
    E: ApplicationError,
    SR: StreamReader<T, E> + Send + 'static,
    SW: StreamWriter<T, E> + Send + 'static,
    BF: BufFactory + 'static,
    BF::Buf: BufSplit + From<Bytes> + Into<Bytes>,
{
    pub fn new(
        id: u64,
        in_direction: Option<(SR, StreamSender<T, E>)>,
        out_direction: Option<(SW, StreamReceiver<T, E>)>,
        ready_futures_sender: UnboundedSender<(u64, ReadyFuture<T, E, SR, SW>)>,
        out_max_buffer_length: usize,
    ) -> Self {
        assert!(
            in_direction.is_some() || out_direction.is_some(),
            "at least one direction must be present"
        );

        let mut stream = Self {
            id,
            open: Arc::new(AtomicBool::new(true)),
            in_state: in_direction.map(|(reader, sender)| InState {
                reader,
                sender,
                draining: false,
            }),
            out_state: out_direction.map(|(writer, receiver)| OutState {
                writer,
                receiver,
                draining: false,
            }),
            out_buffer: Arc::new(Mutex::new(StreamBuffer::new(id))),
            futures: JoinSet::new(),
            in_interrupt_switch: Switch::new(),
            out_interrupt_switch: Switch::new(),
            ready_futures_sender,
            out_max_buffer_length,
            _phantom: PhantomData,
        };

        if let Some(state) = &stream.in_state {
            stream.spawn_future(stream.in_signal_future(state.sender.error_observer()));
        }
        if let Some(state) = stream.out_state.take() {
            stream.spawn_future(stream.out_receive_future(state));
        }

        stream
    }

    pub fn id(&self) -> u64 {
        self.id
    }


    // TODO(docs):
    //  Returns
    //  1) Whether this method updated the connection.
    //  2) Whether this method should be called when 'connection.stream' become writable again.
    pub fn write_pending_bytes_to_wire(
        &mut self,
        connection: &mut Connection<BF>,
    ) -> Result<(bool, bool), ()> {
        if !self.open.load(Ordering::SeqCst) {
            self.close_forced();
            return Err(());
        }

        let (drain_result, length) = {
            let mut buffer = self.lock_out_buffer();
            (buffer.drain(connection), buffer.length())
        };
        match drain_result {
            Ok(drained_something) => {
                if length < self.out_max_buffer_length {
                    if let Some(state) = self.out_state.take() {
                        if state.writer.has_buffer() {
                            self.spawn_future(self.out_writer_next_future(state));
                        } else {
                            self.spawn_future(self.out_receive_future(state));
                        }
                    }
                }

                Ok((drained_something, length != 0))
            }
            Err(e) => {
                match e {
                    Error::StreamStopped(code) => {
                        self.close_out_due_stop_sending(code.into(), None)?;
                    }
                    it => {
                        error!(
                            "failed to write outgoing QUIC stream({}) bytes into quiche::Connection: {}",
                            self.id, it
                        );
                        self.close_out_due_internal_error(connection, None)?;
                    }
                }

                Ok((true, false))
            }
        }
    }

    // TODO(docs):
    //  Returns whether this method updated the connection.
    pub fn read_pending_bytes_from_wire(
        &mut self,
        connection: &mut Connection<BF>,
    ) -> Result<bool, ()> {
        if !self.open.load(Ordering::SeqCst) {
            self.close_forced();
            return Err(());
        }
        let Some(mut state) = self.in_state.take().filter(|it| !it.draining) else {
            return Ok(false);
        };

        let mut read_something = false;
        loop {
            let buffer = state.reader.buffer();
            assert!(
                buffer.len() > 0,
                "'StreamReader' must not return empty buffers"
            );

            let (read, finish) = match connection.stream_recv(self.id, buffer) {
                Ok(it) => it,

                Err(Error::Done) => {
                    self.in_state = Some(state);
                    return Ok(read_something);
                }
                Err(Error::StreamReset(code)) => {
                    self.close_in_due_reset_stream(code.into(), Some(state))?;
                    return Ok(true);
                }
                Err(e) => {
                    error!(
                        "failed to read incoming QUIC stream({}) bytes from quiche::Connection: {}",
                        self.id, e
                    );
                    self.close_in_due_internal_error(connection, Some(state))?;
                    return Ok(true);
                }
            };

            if read == 0 && !finish {
                self.in_state = Some(state);
                return Ok(read_something);
            }

            read_something = true;
            if finish {
                state.draining = true;
            }

            let moved_state = match self
                .poll_or_schedule_future(self.in_reader_notify_future(read, state))
            {
                Some(immediate_result) => self.handle_in_future(immediate_result, connection)?,
                None => {
                    // It might be refactored with 'result.ok()',
                    // but I'm afraid of side effects if the 'Err' type changes.
                    None
                }
            };

            if let Some(it) = moved_state {
                state = it
            } else {
                return Ok(read_something);
            }
        }
    }

    pub fn exec_future(
        &mut self,
        future: ReadyFuture<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<Ready, ()> {
        if !self.open.load(Ordering::SeqCst) {
            self.close_forced();
            return Err(());
        }

        // TODO(refactor): Is there a better way to achieve this, without moving the futures out the 'Stream'?
        //  Polling without race-conditions?
        let result = future.inner;
        let mut in_ready = self.in_state.is_some();
        let mut out_ready = self.out_state.is_some();

        if result.is_in() {
            match self.handle_in_future(result, connection)? {
                Some(state) => {
                    self.in_state = Some(state);
                    in_ready = true;
                }
                None => {
                    in_ready = false;
                }
            }
        } else {
            match self.handle_out_future(result, connection)? {
                Some(state) => {
                    self.out_state = Some(state);
                    out_ready = true;
                }
                None => {
                    out_ready = false;
                }
            }
        }

        Ok(Ready::new(in_ready, out_ready))
    }


    /*
     * Handle future result
     */

    fn handle_in_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<Option<InState<T, E, SR>>, ()> {
        self.handle_future(result, |(stream, result)| {
            let stage_result = match &result {
                #[rustfmt::skip]
                FutureResult::InSignal(_) => {
                    stream.handle_in_signal_future(result, connection)
                },
                FutureResult::InInterrupt { .. } => {
                    stream.handle_in_interrupt_future(result, connection)
                }
                FutureResult::InReaderNotify { .. } => {
                    stream.handle_in_reader_notify_future(result, connection)
                }
                FutureResult::InReaderNext { .. } => {
                    stream.handle_in_reader_next_future(result, connection)
                }
                #[rustfmt::skip]
                FutureResult::InSend { .. } => {
                    stream.handle_in_send_future(result, connection)
                },
                rest => {
                    panic!("unable to handle 'FutureResult({rest})': expecting 'In' futures only");
                }
            };

            stage_result.map(|it| it.into())
        })
    }

    fn handle_out_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<Option<OutState<T, E, SW>>, ()> {
        self.handle_future(result, |(stream, result)| {
            let stage_result = match &result {
                FutureResult::OutInterrupt { .. } => {
                    stream.handle_out_interrupt_future(result, connection)
                }
                FutureResult::OutWriterWrite { .. } => {
                    stream.handle_out_writer_write_future(result, connection)
                }
                FutureResult::OutWriterNext { .. } => {
                    stream.handle_out_writer_next_future(result, connection)
                }
                FutureResult::OutReceive { .. } => {
                    stream.handle_out_receive_future(result, connection)
                }

                rest => {
                    panic!("unable to handle 'FutureResult({rest})': expecting 'Out' futures only");
                }
            };

            stage_result.map(|it| it.into())
        })
    }

    fn handle_future<S, H>(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        mut handler: H,
    ) -> Result<Option<S>, ()>
    where
        H: FnMut((&mut Self, FutureResult<T, E, SR, SW>)) -> Result<Stage<T, E, SR, SW, S>, ()>,
    {
        // Centralize futures handling
        // to prevent stack overflow
        let mut result = result;

        loop {
            result = match handler((self, result))? {
                Stage::Future(future) => {
                    if let Some(immediate_result) = self.poll_or_schedule_future(future) {
                        // TODO(performance):
                        //  What about blocking methods that constantly end up in 'Poll::Ready(_)'?
                        //  What's the overhead?
                        immediate_result
                    } else {
                        return Ok(None);
                    }
                }
                Stage::State(state) => {
                    return Ok(Some(state));
                }
                Stage::Close => {
                    return Ok(None);
                }
            };
        }
    }


    fn handle_in_signal_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<InStage<T, E, SR, SW>, ()> {
        let result = match result {
            FutureResult::InSignal(it) => it,

            rest => {
                panic!("unexpected 'FutureResult'({rest}): expecting 'InSignal' result");
            }
        };

        let send_error = match result {
            Ok(it) => it,
            Err(interrupt) => {
                if let Some(state) = self.in_state.take() {
                    self.close_in_due_direction_error(interrupt, connection, state)?;
                }

                return Ok(InStage::Close);
            }
        };

        let direction_error = match send_error {
            SendError::HangUp => DirectionError::Internal,
            SendError::StopSending(code) => DirectionError::StopSending(code),
            SendError::StreamWriter(_) => {
                unreachable!("only peer's application can receive 'StreamWriter' error");
            }
        };

        if self.in_interrupt_switch.try_switch(direction_error).is_ok() {
            if let Some(state) = self.in_state.take() {
                self.close_in_due_direction_error(direction_error, connection, state)?;
            }
        }

        Ok(InStage::Close)
    }

    fn handle_in_reader_notify_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<InStage<T, E, SR, SW>, ()> {
        let state = match result {
            FutureResult::InReaderNotify { state, result } => {
                if let Err(code) = result {
                    self.close_in_due_mapper_error(code, connection, Some(state))?;
                    return Ok(InStage::Close);
                }

                state
            }

            rest => {
                return self.handle_in_interrupt_future(rest, connection);
            }
        };

        if state.reader.has_message() {
            return Ok(InStage::Future(self.in_reader_next_future(state)));
        }

        if state.draining {
            Ok(InStage::Future(self.in_send_future(None, state)))
        } else {
            Ok(InStage::State(state))
        }
    }

    fn handle_in_reader_next_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<InStage<T, E, SR, SW>, ()> {
        let (state, message) = match result {
            FutureResult::InReaderNext { state, result } => match result {
                Ok(message) => (state, message),
                Err(code) => {
                    self.close_in_due_mapper_error(code, connection, Some(state))?;
                    return Ok(InStage::Close);
                }
            },

            rest => {
                return self.handle_in_interrupt_future(rest, connection);
            }
        };

        Ok(InStage::Future(self.in_send_future(Some(message), state)))
    }

    fn handle_in_send_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<InStage<T, E, SR, SW>, ()> {
        let state = match result {
            FutureResult::InSend { state, result } => {
                if let Err(e) = result {
                    match e {
                        SendError::HangUp => {
                            warn!(
                                "detected incoming QUIC stream({}) channel hang-up: \
                                closing direction with the 'internal_error'({})",
                                self.id,
                                E::internal()
                            );
                            self.close_in_due_internal_error(connection, Some(state))?;
                        }
                        SendError::StopSending(code) => {
                            self.close_in_due_stop_sending(code, connection, Some(state))?;
                        }
                        SendError::StreamWriter(_) => {
                            unreachable!(
                                "only peer's application can receive 'StreamWriter' error"
                            );
                        }
                    }

                    return Ok(InStage::Close);
                }

                state
            }

            rest => {
                return self.handle_in_interrupt_future(rest, connection);
            }
        };

        if state.reader.has_message() {
            return Ok(InStage::Future(self.in_reader_next_future(state)));
        }

        Ok(InStage::State(state))
    }

    fn handle_in_interrupt_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<InStage<T, E, SR, SW>, ()> {
        match result {
            FutureResult::InInterrupt { state, error } => {
                self.close_in_due_direction_error(error, connection, state)?;
                Ok(InStage::Close)
            }
            rest => {
                panic!("unexpected 'FutureResult'({rest}): expecting 'InInterrupt' result");
            }
        }
    }


    fn handle_out_receive_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<OutStage<T, E, SR, SW>, ()> {
        let (mut state, message, finish) = match result {
            FutureResult::OutReceive { state, result } => match result {
                Ok((message, finish)) => (state, message, finish),
                Err(RecvError::Finish) => (state, None, true),

                Err(e) => {
                    match e {
                        RecvError::HangUp => {
                            warn!(
                                "detected outgoing QUIC stream({}) channel hang-up: \
                                closing direction with the 'internal_error'({})",
                                self.id,
                                E::internal()
                            );
                            self.close_out_due_internal_error(connection, Some(state))?;
                        }
                        RecvError::ResetStream(code) => {
                            self.close_out_due_reset_stream(code, connection, Some(state))?;
                        }
                        RecvError::StreamReader(_) => {
                            unreachable!(
                                "only peer's application can receive 'StreamReader' error"
                            );
                        }

                        RecvError::Finish => {
                            unreachable!("Finish is handled above");
                        }
                    };

                    return Ok(OutStage::Close);
                }
            },

            rest => {
                return self.handle_out_interrupt_future(rest, connection);
            }
        };

        if let Some(priority) = state.receiver.priority_once() {
            let _ = connection.stream_priority(self.id, priority.urgency, priority.incremental);
        }

        match (message, finish) {
            (Some(message), false) => Ok(OutStage::Future(
                self.out_writer_write_future(message, state),
            )),
            (Some(message), true) => {
                state.draining = true;
                Ok(OutStage::Future(
                    self.out_writer_write_future(message, state),
                ))
            }
            (None, true) => {
                state.draining = true;
                self.lock_out_buffer().finish();
                Ok(OutStage::State(state))
            }
            (None, false) => {
                unreachable!("'StreamReceiver' result cannot be equal to 'Ok(None, false)'");
            }
        }
    }

    fn handle_out_writer_write_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<OutStage<T, E, SR, SW>, ()> {
        let mut state = match result {
            FutureResult::OutWriterWrite { state, result } => {
                if let Err(code) = result {
                    self.close_out_due_mapper_error(code, connection, Some(state))?;
                    return Ok(OutStage::Close);
                }

                state
            }

            rest => {
                return self.handle_out_interrupt_future(rest, connection);
            }
        };

        if state.writer.has_buffer() {
            return Ok(OutStage::Future(self.out_writer_next_future(state)));
        }
        if state.draining {
            self.lock_out_buffer().finish();
        }

        // If the state is not 'draining'
        // and no buffer is available after a 'writer.write(message)' call,
        // it is assumed that the message was cached/stored and it was not lost.
        Ok(OutStage::State(state))
    }

    fn handle_out_writer_next_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<OutStage<T, E, SR, SW>, ()> {
        let (mut state, bytes) = match result {
            FutureResult::OutWriterNext { state, result } => match result {
                Ok(bytes) => (state, bytes),
                Err(code) => {
                    self.close_out_due_mapper_error(code, connection, Some(state))?;
                    return Ok(OutStage::Close);
                }
            },

            rest => {
                return self.handle_out_interrupt_future(rest, connection);
            }
        };

        let mut buffer = self.lock_out_buffer();
        buffer.append(bytes);

        match (state.draining, state.writer.has_buffer()) {
            (true, false) => {
                buffer.finish();
                Ok(OutStage::Close)
            }
            (false, false) => {
                // #[rustfmt::skip]
                Ok(OutStage::State(state))
            }
            (_, true) => {
                if buffer.length() < self.out_max_buffer_length {
                    Ok(OutStage::Future(self.out_writer_next_future(state)))
                } else {
                    Ok(OutStage::State(state))
                }
            }
        }
    }

    fn handle_out_interrupt_future(
        &mut self,
        result: FutureResult<T, E, SR, SW>,
        connection: &mut Connection<BF>,
    ) -> Result<OutStage<T, E, SR, SW>, ()> {
        match result {
            FutureResult::OutInterrupt { state, error } => {
                self.close_out_due_direction_error(error, connection, state)?;
                Ok(OutStage::Close)
            }
            rest => {
                panic!("unexpected 'FutureResult'({rest}): expecting 'OutInterrupt'");
            }
        }
    }


    fn poll_or_schedule_future<F>(&mut self, mut future: F) -> Option<FutureResult<T, E, SR, SW>>
    where
        F: Future<Output = FutureResult<T, E, SR, SW>> + Send + Unpin + 'static,
    {
        match poll(&mut future) {
            Poll::Ready(result) => Some(result),
            Poll::Pending => {
                self.spawn_future(future);
                None
            }
        }
    }


    /*
     * Futures
     *
     * TODO(refactor): write macro to avoid 'observer.switched()' repetition?
     */

    fn in_signal_future(
        &self,
        sender_error_observer: Observer<Switch<SendError<E>>>,
    ) -> StreamFuture<T, E, SR, SW> {
        let interruption_observer = self.in_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = interruption_observer.switched() => {
                    FutureResult::InSignal(Err(error))
                },
                error = sender_error_observer.switched() => {
                    FutureResult::InSignal(Ok(error))
                }
            }
        })
    }

    fn in_reader_notify_future(
        &self,
        length: usize,
        mut state: InState<T, E, SR>,
    ) -> StreamFuture<T, E, SR, SW> {
        let observer = self.in_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = observer.switched() => {
                    FutureResult::InInterrupt { state, error }
                },
                result = state.reader.notify_read(length, state.draining) => {
                    FutureResult::InReaderNotify { state, result }
                },
            }
        })
    }

    fn in_reader_next_future(&self, mut state: InState<T, E, SR>) -> StreamFuture<T, E, SR, SW> {
        let observer = self.in_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = observer.switched() => {
                    FutureResult::InInterrupt { state, error }
                },
                result = state.reader.next_message() => {
                    FutureResult::InReaderNext { state, result }
                },
            }
        })
    }

    fn in_send_future(
        &self,
        message: Option<T>,
        mut state: InState<T, E, SR>,
    ) -> StreamFuture<T, E, SR, SW> {
        let observer = self.in_interruption_observer();
        let finish_now = state.draining && !state.reader.has_message();

        match (message, finish_now) {
            (Some(message), false) => Box::pin(async move {
                select! {
                    biased;

                    error = observer.switched() => {
                        FutureResult::InInterrupt { state, error }
                    },
                    result = state.sender.send(message) => {
                        FutureResult::InSend { state, result }
                    }
                }
            }),
            (Some(message), true) => Box::pin(async move {
                select! {
                    biased;

                    error = observer.switched() => {
                        FutureResult::InInterrupt { state, error }
                    },
                    result = state.sender.send_and_finish_keep(message) => {
                        FutureResult::InSend { state, result }
                    }
                }
            }),
            (None, true) => Box::pin(async move {
                select! {
                    biased;

                    error = observer.switched() => {
                        FutureResult::InInterrupt { state, error }
                    },
                    result = state.sender.finish_keep() => {
                        FutureResult::InSend { state, result }
                    }
                }
            }),
            (None, false) => {
                panic!("cannot send (message:None, finish:false)");
            }
        }
    }


    fn out_writer_write_future(
        &self,
        message: T,
        mut state: OutState<T, E, SW>,
    ) -> StreamFuture<T, E, SR, SW> {
        let observer = self.out_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = observer.switched() => {
                    FutureResult::OutInterrupt { state, error }
                },
                result = state.writer.write(message, state.draining) => {
                    FutureResult::OutWriterWrite { state, result }
                },
            }
        })
    }

    fn out_writer_next_future(&self, mut state: OutState<T, E, SW>) -> StreamFuture<T, E, SR, SW> {
        let observer = self.out_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = observer.switched() => {
                    FutureResult::OutInterrupt { state, error }
                },
                result = state.writer.next_buffer() => {
                    FutureResult::OutWriterNext { state, result }
                },
            }
        })
    }

    fn out_receive_future(&self, mut state: OutState<T, E, SW>) -> StreamFuture<T, E, SR, SW> {
        let observer = self.out_interruption_observer();
        Box::pin(async move {
            select! {
                biased;

                error = observer.switched() => {
                    FutureResult::OutInterrupt { state, error }
                },
                result = state.receiver.recv_full() => {
                    FutureResult::OutReceive { state, result }
                },
            }
        })
    }


    fn spawn_future<F>(&mut self, future: F)
    where
        F: Future<Output = FutureResult<T, E, SR, SW>> + Send + Unpin + 'static,
    {
        // TODO(performance):
        //  What's the overhead?
        let open = self.open.clone();
        let stream_id = self.id;
        let ready_futures_sender = self.ready_futures_sender.clone();
        let in_interrupt_switch = self.in_interrupt_switch.clone();
        let out_interrupt_switch = self.out_interrupt_switch.clone();

        self.futures.spawn(async move {
            let result = future.await;

            if ready_futures_sender
                .send((stream_id, result.into()))
                .is_err()
            {
                error!("failed to send QUIC stream 'ReadyFuture': channel hang-up");
                open.store(false, Ordering::SeqCst);

                let _ = in_interrupt_switch.try_switch(DirectionError::Internal);
                let _ = out_interrupt_switch.try_switch(DirectionError::Internal);
            };
        });
    }


    /*
     * Error handling
     */

    fn close_in_due_internal_error(
        &mut self,
        connection: &mut Connection<BF>,
        state: Option<InState<T, E, SR>>,
    ) -> Result<(), ()> {
        let _ = self
            .in_interrupt_switch
            .try_switch(DirectionError::Internal);

        self.shutdown_quiche_stream(E::internal(), Shutdown::Read, connection);
        self.take_in_state(state); // HangUp
        self.close_stream_if_unused()
    }

    // wire >>> bytes >>> StreamReader error
    #[rustfmt::skip]
    fn close_in_due_mapper_error(
        &mut self,
        code: E,
        connection: &mut Connection<BF>,
        state: Option<InState<T, E, SR>>,
    ) -> Result<(), ()> {
        let _ = self
            .in_interrupt_switch
            .try_switch(DirectionError::StreamMapper(code));

        self.shutdown_quiche_stream(code, Shutdown::Read, connection);
        self.take_in_state(state).sender.stream_reader_error(code);
        self.close_stream_if_unused()
    }

    // wire >>> RESET_STREAM(code) >>> peer
    fn close_in_due_reset_stream(
        &mut self,
        code: E,
        state: Option<InState<T, E, SR>>,
    ) -> Result<(), ()> {
        let _ = self
            .in_interrupt_switch
            .try_switch(DirectionError::ResetStream(code));

        self.take_in_state(state).sender.reset_stream(code);
        self.close_stream_if_unused()
    }

    // wire <<< STOP_SENDING(code) <<< peer
    fn close_in_due_stop_sending(
        &mut self,
        code: E,
        connection: &mut Connection<BF>,
        state: Option<InState<T, E, SR>>,
    ) -> Result<(), ()> {
        let _ = self
            .in_interrupt_switch
            .try_switch(DirectionError::StopSending(code));

        self.shutdown_quiche_stream(code, Shutdown::Read, connection);
        self.take_in_state(state);
        self.close_stream_if_unused()
    }

    #[rustfmt::skip]
    fn close_in_due_direction_error(
        &mut self,
        error: DirectionError<E>,
        connection: &mut Connection<BF>,
        state: InState<T, E, SR>,
    ) -> Result<(), ()> {
        match error {
            DirectionError::Internal => {
                self.close_in_due_internal_error(connection, Some(state))
            }
            DirectionError::StopSending(code) => {
                self.close_in_due_stop_sending(code, connection, Some(state))
            }
            DirectionError::ResetStream(code) => {
                self.close_in_due_reset_stream(code, Some(state))
            }
            DirectionError::StreamMapper(code) => {
                self.close_in_due_mapper_error(code, connection, Some(state))
            }
            DirectionError::Unused => {
                unreachable!("future cannot be interrupted with the 'Unused' error");
            }
        }
    }

    fn take_in_state(&mut self, or: Option<InState<T, E, SR>>) -> InState<T, E, SR> {
        self.in_state
            .take()
            .or(or)
            .unwrap_or_else(|| self.panic_state_leak("in"))
    }


    fn close_out_due_internal_error(
        &mut self,
        connection: &mut Connection<BF>,
        state: Option<OutState<T, E, SW>>,
    ) -> Result<(), ()> {
        let _ = self
            .out_interrupt_switch
            .try_switch(DirectionError::Internal);

        self.shutdown_quiche_stream(E::internal(), Shutdown::Write, connection);
        self.take_out_state(state); // HangUp
        self.close_stream_if_unused()
    }

    // StreamWriter error <<< message(T) <<< peer
    #[rustfmt::skip]
    fn close_out_due_mapper_error(
        &mut self,
        code: E,
        connection: &mut Connection<BF>,
        state: Option<OutState<T, E, SW>>,
    ) -> Result<(), ()> {
        let _ = self
            .out_interrupt_switch
            .try_switch(DirectionError::StreamMapper(code));

        self.shutdown_quiche_stream(code, Shutdown::Write, connection);
        self.take_out_state(state).receiver.stream_writer_error(code);
        self.close_stream_if_unused()
    }

    // wire <<< RESET_STREAM(code) <<< peer
    fn close_out_due_reset_stream(
        &mut self,
        code: E,
        connection: &mut Connection<BF>,
        state: Option<OutState<T, E, SW>>,
    ) -> Result<(), ()> {
        let _ = self
            .out_interrupt_switch
            .try_switch(DirectionError::ResetStream(code));

        self.shutdown_quiche_stream(code, Shutdown::Write, connection);
        self.take_out_state(state);
        self.close_stream_if_unused()
    }

    // wire >>> STOP_SENDING(code) >>> peer
    fn close_out_due_stop_sending(
        &mut self,
        code: E,
        state: Option<OutState<T, E, SW>>,
    ) -> Result<(), ()> {
        let _ = self
            .out_interrupt_switch
            .try_switch(DirectionError::StopSending(code));

        self.take_out_state(state).receiver.stop_sending(code);
        self.close_stream_if_unused()
    }

    #[rustfmt::skip]
    fn close_out_due_direction_error(
        &mut self,
        error: DirectionError<E>,
        connection: &mut Connection<BF>,
        state: OutState<T, E, SW>,
    ) -> Result<(), ()> {
        match error {
            DirectionError::Internal => {
                self.close_out_due_internal_error(connection, Some(state))
            },
            DirectionError::StopSending(code) => {
                self.close_out_due_stop_sending(code, Some(state))
            },
            DirectionError::ResetStream(code) => {
                self.close_out_due_reset_stream(code, connection, Some(state))
            }
            DirectionError::StreamMapper(code) => {
                self.close_out_due_mapper_error(code, connection, Some(state))
            }
            DirectionError::Unused => {
                unreachable!("future cannot be interrupted with the 'Unused' error");
            }
        }
    }

    fn take_out_state(&mut self, or: Option<OutState<T, E, SW>>) -> OutState<T, E, SW> {
        self.out_state
            .take()
            .or(or)
            .unwrap_or_else(|| self.panic_state_leak("out"))
    }


    fn close_stream_if_unused(&mut self) -> Result<(), ()> {
        if self.in_state.is_none()
            && self.out_state.is_none()
            && self.in_interrupt_switch.is_switched()
            && self.out_interrupt_switch.is_switched()
        {
            self.open.store(false, Ordering::SeqCst);
            Err(())
        } else {
            Ok(())
        }
    }

    fn close_forced(&mut self) {
        self.open.store(false, Ordering::SeqCst);
        self.in_state = None;
        self.out_state = None;

        let _ = self
            .in_interrupt_switch
            .try_switch(DirectionError::Internal);
        let _ = self
            .out_interrupt_switch
            .try_switch(DirectionError::Internal);
    }


    fn lock_out_buffer(&self) -> MutexGuard<'_, StreamBuffer> {
        // TODO(refactor): close 'out' direction instead?
        self.out_buffer
            .lock()
            .expect("failed to lock outgoing QUIC stream buffer: mutex is poisoned")
    }

    fn shutdown_quiche_stream(
        &self,
        code: E,
        direction: Shutdown,
        connection: &mut Connection<BF>,
    ) {
        if let Err(e) = connection.stream_shutdown(self.id, direction, code.into()) {
            if e != Error::Done {
                panic!(
                    "failed to shutdown QUIC stream({}) in quiche::Connection: {}",
                    self.id, e
                );
            }
        }
    }

    fn panic_state_leak(&self, direction: &'static str) -> ! {
        panic!(
            "'{}' state leak: it must either be stored in 'self.state', \
            or provided directly through 'state' variable. \
            [stream_id: {}]",
            direction, self.id
        )
    }


    /*
     * Utils
     */

    fn in_interruption_observer(&self) -> Observer<Switch<DirectionError<E>>> {
        self.in_interrupt_switch.clone().into()
    }

    fn out_interruption_observer(&self) -> Observer<Switch<DirectionError<E>>> {
        self.out_interrupt_switch.clone().into()
    }
}


// TODO(docs):
//  peer(our application) <<< wire.
struct InState<T, E, SR>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
{
    reader: SR,
    sender: StreamSender<T, E>,
    draining: bool,
}

// TODO(docs):
//  peer(our application) >>> wire.
struct OutState<T, E, SW>
where
    E: ApplicationError,
    SW: StreamWriter<T, E>,
{
    writer: SW,
    receiver: StreamReceiver<T, E>,
    draining: bool,
}


/*
 * Future
 */

#[derive(Debug, Copy, Clone)]
enum DirectionError<E> {
    Unused,
    Internal,
    StopSending(E),
    ResetStream(E),
    StreamMapper(E),
}


type StreamFuture<T, E, SR, SW> = Pin<Box<dyn Future<Output = FutureResult<T, E, SR, SW>> + Send>>;

enum Stage<T, E, SR, SW, S>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    Future(StreamFuture<T, E, SR, SW>),
    State(S),
    Close,
}

impl<T, E, SR, SW> From<InStage<T, E, SR, SW>> for Stage<T, E, SR, SW, InState<T, E, SR>>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    fn from(value: InStage<T, E, SR, SW>) -> Self {
        match value {
            InStage::Future(future) => Stage::Future(future),
            InStage::State(state) => Stage::State(state),
            InStage::Close => Stage::Close,
        }
    }
}

impl<T, E, SR, SW> From<OutStage<T, E, SR, SW>> for Stage<T, E, SR, SW, OutState<T, E, SW>>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    fn from(value: OutStage<T, E, SR, SW>) -> Self {
        match value {
            OutStage::Future(future) => Stage::Future(future),
            OutStage::State(state) => Stage::State(state),
            OutStage::Close => Stage::Close,
        }
    }
}

enum InStage<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    Future(StreamFuture<T, E, SR, SW>),
    State(InState<T, E, SR>),
    Close,
}

enum OutStage<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    Future(StreamFuture<T, E, SR, SW>),
    State(OutState<T, E, SW>),
    Close,
}


// TODO(docs):
//  `In` futures must not depend on `Out`, and vice-versa.
enum FutureResult<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    InSignal(Result<SendError<E>, DirectionError<E>>),

    InInterrupt {
        state: InState<T, E, SR>,
        error: DirectionError<E>,
    },
    InReaderNotify {
        state: InState<T, E, SR>,
        result: Result<(), E>,
    },
    InReaderNext {
        state: InState<T, E, SR>,
        result: Result<T, E>,
    },
    InSend {
        state: InState<T, E, SR>,
        result: Result<(), SendError<E>>,
    },

    OutInterrupt {
        state: OutState<T, E, SW>,
        error: DirectionError<E>,
    },
    OutWriterWrite {
        state: OutState<T, E, SW>,
        result: Result<(), E>,
    },
    OutWriterNext {
        state: OutState<T, E, SW>,
        result: Result<Bytes, E>,
    },
    OutReceive {
        state: OutState<T, E, SW>,
        result: Result<(Option<T>, bool), RecvError<E>>,
    },
}

impl<T, E, SR, SW> FutureResult<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    fn is_in(&self) -> bool {
        match self {
            FutureResult::InSignal(_)
            | FutureResult::InInterrupt { .. }
            | FutureResult::InReaderNotify { .. }
            | FutureResult::InReaderNext { .. }
            | FutureResult::InSend { .. } => true,

            _ => false,
        }
    }

    fn is_out(&self) -> bool {
        !self.is_in()
    }
}

impl<T, E, SR, SW> Display for FutureResult<T, E, SR, SW>
where
    E: ApplicationError,
    SR: StreamReader<T, E>,
    SW: StreamWriter<T, E>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FutureResult::InSignal(_) => write!(f, "InSignal"),
            FutureResult::InInterrupt { .. } => write!(f, "InInterrupt"),
            FutureResult::InReaderNotify { .. } => write!(f, "InReaderNotify"),
            FutureResult::InReaderNext { .. } => write!(f, "InReaderNext"),
            FutureResult::InSend { .. } => write!(f, "InSend"),
            FutureResult::OutInterrupt { .. } => write!(f, "OutInterrupt"),
            FutureResult::OutWriterWrite { .. } => write!(f, "OutWriterWrite"),
            FutureResult::OutWriterNext { .. } => write!(f, "OutWriterNext"),
            FutureResult::OutReceive { .. } => write!(f, "OutReceive"),
        }
    }
}
