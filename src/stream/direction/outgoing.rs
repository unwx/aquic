use crate::ApplicationError;
use crate::buffer::BufViewFactory;
use crate::stream::{Encoder, Outcome, Receiver, WriteStream};
use crate::stream::buffer::StreamBuffer;
use crate::stream::direction::exec::{Executor, OrchestrateResult, orchestrate_futures};
use crate::stream::direction::{DirectionError, exec};
use bytes::Bytes;
use quiche::Error;
use tracing::{debug, error};

pub struct CompletedFuture<T, SE, E> {
    state: State<T, SE, E>,
    result: Result<FutureResult<T, E>, DirectionError<E>>,
}

impl<T, SE, E> exec::CompletedFuture<State<T, SE, E>, FutureResult<T, E>, E>
    for CompletedFuture<T, SE, E>
{
    fn from(state: State<T, SE, E>, result: Result<FutureResult<T, E>, DirectionError<E>>) -> Self {
        Self { state, result }
    }

    fn into_tuple(
        self,
    ) -> (
        State<T, SE, E>,
        Result<FutureResult<T, E>, DirectionError<E>>,
    ) {
        (self.state, self.result)
    }
}


pub struct Outgoing<T, SE, E> {
    state: Option<State<T, SE, E>>,
    executor: Executor<CompletedFuture<T, SE, E>, E>,
    open: bool,
}

impl<T, SE, E> Outgoing<T, SE, E>
where
    T: Send + 'static,
    E: ApplicationError,
    SE: Encoder<T, E> + Send + 'static,
{
    pub fn new(
        stream_encoder: SE,
        stream_receiver: Receiver<T, E>,
        executor: Executor<CompletedFuture<T, SE, E>, E>,
    ) -> Self {
        Self {
            state: Some(State {
                encoder: stream_encoder,
                receiver: stream_receiver,
                buffer: StreamBuffer::new(),
                finish: false,
            }),
            executor,
            open: true,
        }
    }


    // TODO(docs):
    //  Ok(true, _): connection needs to be flushed.
    //  Ok(_, true): ready for the next `write_to_stream()`
    //  Err(()): direction is closed.
    pub fn write_to_stream(
        &mut self,
        stream: &mut WriteStream<BufViewFactory>,
    ) -> Result<(bool, bool), ()> {
        if !self.open {
            return Err(());
        }

        let Some(mut state) = self.state.take() else {
            return Ok((false, false));
        };

        if let Some(priority) = state.receiver.priority_once() {
            let _ = stream.priority(priority.urgency, priority.incremental);
        }

        let mut wrote_something = false;
        loop {
            match state.buffer.drain(stream) {
                Ok(true) => {
                    wrote_something = true;
                    continue;
                }

                Ok(false) => {
                    if state.finish && state.buffer.is_empty() {
                        self.close(DirectionError::Finish, state, stream);
                        return Err(());
                    }

                    self.state = Some(state);
                    return Ok((wrote_something, true));
                }

                Err(Error::StreamStopped(code)) => {
                    self.close(DirectionError::StopSending(code.into()), state, stream);
                    return Err(());
                }

                Err(e) => {
                    error!(
                        "failed to write outgoing QUIC stream({}) bytes: {}",
                        stream.id(),
                        e
                    );
                    self.close(DirectionError::Internal, state, stream);
                    return Err(());
                }
            }
        }
    }

    // TODO(docs):
    //  Ok(true): ready for the `write_to_stream()`.
    //  Err(()): direction is closed.
    pub fn consume_completed_future(
        &mut self,
        completed_future: CompletedFuture<T, SE, E>,
        stream: &mut WriteStream<BufViewFactory>,
    ) -> Result<bool, ()> {
        debug_assert!(self.state.is_none());

        if !self.open {
            return Err(());
        }

        match orchestrate_futures(
            completed_future.state,
            completed_future.result,
            &self.executor,
            Self::handle_future_result,
        ) {
            OrchestrateResult::Complete(state) => {
                if state.finish && state.buffer.is_empty() {
                    self.close(DirectionError::Finish, state, stream);
                    return Err(());
                }

                self.state = Some(state);
                Ok(true)
            }
            OrchestrateResult::Schedule => Ok(false),
            OrchestrateResult::Error(state, e) => {
                self.close(e, state, stream);
                Err(())
            }
        }
    }


    /*
     * Handle future result
     */

    fn handle_future_result(
        result: FutureResult<T, E>,
        state: &mut State<T, SE, E>,
    ) -> StageResult<T, E> {
        match result {
            FutureResult::ReceiverRecv(result) => Self::handle_receiver_recv(result, state),
            FutureResult::MapperWrite(result) => Self::handle_mapper_write(result, state),
            FutureResult::MapperNext(result) => Self::handle_mapper_next(result, state),
        }
    }

    fn handle_receiver_recv(
        result: Result<Option<T>, Outcome<E>>,
        state: &State<T, SE, E>,
    ) -> StageResult<T, E> {
        let message = match result {
            Ok(it) => it,
            Err(Outcome::Finish) => None,

            Err(e) => {
                #[rustfmt::skip]
                let error = todo!();
                return Err(error);
            }
        };

        if message.is_none() && !state.finish {
            panic!("cannot send (message:None, finish:false)");
        }

        Ok(Some(PreparedFuture::MapperWrite(message)))
    }

    fn handle_mapper_write(
        result: Result<(), E>,
        state: &mut State<T, SE, E>,
    ) -> StageResult<T, E> {
        result.map_err(|code| DirectionError::StreamMapper(code))?;

        if state.finish && !state.encoder.has_buffer() {
            state.buffer.finish();
            return Ok(None);
        }

        if state.encoder.has_buffer() {
            Ok(Some(PreparedFuture::MapperNext))
        } else {
            Ok(None)
        }
    }

    fn handle_mapper_next(
        result: Result<Bytes, E>,
        state: &mut State<T, SE, E>,
    ) -> StageResult<T, E> {
        let bytes = result.map_err(|code| DirectionError::StreamMapper(code))?;
        state.buffer.append(bytes);

        if state.finish && !state.encoder.has_buffer() {
            state.buffer.finish();
            return Ok(None);
        }

        if state.encoder.has_buffer() {
            Ok(Some(PreparedFuture::MapperNext))
        } else {
            Ok(None)
        }
    }


    fn close(
        &mut self,
        error: DirectionError<E>,
        state: State<T, SE, E>,
        stream: &mut WriteStream<BufViewFactory>,
    ) {
        self.open = false;
        let _ = self.executor.cancel_sender().send(error);

        match error {
            DirectionError::Finish => {}

            DirectionError::Internal => {
                let _ = stream.shutdown(E::internal());
            }

            // wire >>> STOP_SENDING(code) >>> our application
            DirectionError::StopSending(code) => {
                state.receiver.terminate(code);
            }

            // wire <<< RESET_STREAM(code) <<< our application
            DirectionError::ResetStream(code) => {
                let _ = stream.shutdown(code);
            }

            // StreamWriter error <<< message <<< our application
            DirectionError::StreamMapper(code) => {
                state.receiver.terminate_codec(code);
                let _ = stream.shutdown(code);
            }
        }
    }
}


type StageResult<T, E> = Result<Option<PreparedFuture<T>>, DirectionError<E>>;

struct State<T, SE, E> {
    encoder: SE,
    receiver: Receiver<T, E>,
    buffer: StreamBuffer,
    finish: bool,
}

enum PreparedFuture<T> {
    ReceiverRecv,
    MapperWrite(Option<T>),
    MapperNext,
}

enum FutureResult<T, E> {
    ReceiverRecv(Result<Option<T>, Outcome<E>>),
    MapperWrite(Result<(), E>),
    MapperNext(Result<Bytes, E>),
}

impl<T, SE, E> exec::PreparedFuture<State<T, SE, E>, FutureResult<T, E>> for PreparedFuture<T>
where
    T: Send,
    E: ApplicationError,
    SE: Encoder<T, E> + Send,
{
    async fn run(self, state: &mut State<T, SE, E>) -> FutureResult<T, E> {
        match self {
            PreparedFuture::ReceiverRecv => FutureResult::ReceiverRecv(
                state
                    .receiver
                    .recv()
                    .await
                    .map(|item| {
                        state.finish = state.finish || item.is_finish();
                        item.into_inner()
                    })
                    .inspect_err(|e| {
                        if matches!(e, Outcome::Finish) {
                            state.finish = true;
                        }
                    }),
            ),

            PreparedFuture::MapperWrite(message) => {
                FutureResult::MapperWrite(state.encoder.write(message, state.finish).await)
            }

            PreparedFuture::MapperNext => {
                FutureResult::MapperNext(state.encoder.next_buffer().await)
            }
        }
    }
}
