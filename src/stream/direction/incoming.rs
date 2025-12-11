use crate::ApplicationError;
use crate::buffer::BufViewFactory;
use crate::stream::{Outcome, ReadStream, Sender};
use crate::stream::direction::exec::{
    Executor, OrchestrateResult, exec_cancellable, orchestrate_futures, try_complete_now,
};
use crate::stream::direction::{DirectionError, exec};
use crate::stream::codec::StreamDecoder;
use quiche::Error;
use std::any::type_name;
use std::borrow::Cow;
use tracing::{debug, error};

pub struct CompletedFuture<T, SD, E> {
    state: State<T, SD, E>,
    result: Result<FutureResult<T, E>, DirectionError<E>>,
}

impl<T, SD, E> exec::CompletedFuture<State<T, SD, E>, FutureResult<T, E>, E>
    for CompletedFuture<T, SD, E>
{
    fn from(state: State<T, SD, E>, result: Result<FutureResult<T, E>, DirectionError<E>>) -> Self {
        Self { state, result }
    }

    fn into_tuple(
        self,
    ) -> (
        State<T, SD, E>,
        Result<FutureResult<T, E>, DirectionError<E>>,
    ) {
        (self.state, self.result)
    }
}


pub struct Incoming<T, SD, E> {
    state: Option<State<T, SD, E>>,
    executor: Executor<CompletedFuture<T, SD, E>, E>,
    open: bool,
}

impl<T, SD, E> Incoming<T, SD, E>
where
    T: Send + 'static,
    E: ApplicationError,
    SD: StreamDecoder<T, E> + Send + 'static,
{
    pub fn new(
        stream_decoder: SD,
        stream_sender: Sender<T, E>,
        executor: Executor<CompletedFuture<T, SD, E>, E>,
    ) -> Self {
        {
            let sender_error_receiver = stream_sender.outcome_receiver();
            let executor = executor.clone();

            tokio::spawn(async move {
                let result = exec_cancellable(
                    async {
                        sender_error_receiver
                            .recv()
                            .await
                            .unwrap_or(Outcome::HangUp(Cow::Borrowed("StreamSender became unavailable")))
                    },
                    executor.cancel_receiver().clone(),
                )
                .await;

                let error = match result {
                    Ok(e) => Self::send_to_direction_err(e),
                    Err(e) => e,
                };

                let _ = executor.cancel_sender().send(error);
            });
        }

        Self {
            state: Some(State {
                decoder: stream_decoder,
                sender: stream_sender,
                finish: false,
            }),
            executor,
            open: true,
        }
    }


    // TODO(docs):
    //  Ok(true, _): connection needs to be flushed.
    //  Ok(_, true): ready for the `read_from_stream()`.
    //  Err(()): direction is closed.
    pub fn read_from_stream(
        &mut self,
        stream: &mut ReadStream<BufViewFactory>,
    ) -> Result<(bool, bool), ()> {
        if !self.open {
            return Err(());
        }

        let Some(mut state) = self.state.take() else {
            return Ok((false, false));
        };

        let mut read_something = false;
        loop {
            if state.finish {
                self.close(DirectionError::Finish, state, stream);
                return Err(());
            }

            let buffer = state.decoder.buffer();
            assert!(
                buffer.len() > 0,
                "'{}' must not produce empty buffers",
                type_name::<SD>()
            );

            let (read, finish) = match stream.recv(buffer) {
                Ok(it) => it,

                Err(Error::Done) => {
                    self.state = Some(state);
                    return Ok((read_something, true));
                }
                Err(Error::StreamReset(code)) => {
                    self.close(DirectionError::ResetStream(code.into()), state, stream);
                    return Err(());
                }

                Err(e) => {
                    error!(
                        "failed to read incoming QUIC stream({}) bytes from quiche::Connection: {}",
                        stream.id(),
                        e
                    );

                    self.close(DirectionError::Internal, state, stream);
                    return Err(());
                }
            };

            if read == 0 && !finish {
                self.state = Some(state);
                return Ok((read_something, true));
            }

            state.finish = state.finish || finish;
            read_something = true;

            let ready_for_next =
                match try_complete_now(state, PreparedFuture::MapperNotify(read), &self.executor) {
                    Some(completed_future) => {
                        self.consume_completed_future(completed_future, stream)?
                    }
                    None => false,
                };

            if !ready_for_next {
                return Ok((read_something, false));
            }

            state = self
                .state
                .take()
                .expect("'state' must be present after 'ready_for_next' is true");
        }
    }

    // TODO(docs):
    //  Ok(true): ready for the `read_from_wire()`.
    //  Err(()): direction is closed.
    pub fn consume_completed_future(
        &mut self,
        completed_future: CompletedFuture<T, SD, E>,
        stream: &mut ReadStream<BufViewFactory>,
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
                if state.finish {
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
        state: &mut State<T, SD, E>,
    ) -> StageResult<T, E> {
        match result {
            FutureResult::MapperNotify(result) => Self::handle_mapper_notify(result, state),
            FutureResult::MapperNext(result) => Self::handle_mapper_next(result),
            FutureResult::SenderSend(result) => Self::handle_sender_send(result, state),
        }
    }

    fn handle_mapper_notify(result: Result<(), E>, state: &State<T, SD, E>) -> StageResult<T, E> {
        result.map_err(|code| DirectionError::StreamMapper(code))?;

        if state.decoder.has_message() {
            return Ok(Some(PreparedFuture::MapperNext));
        }

        if state.finish {
            Ok(Some(PreparedFuture::SenderSend(None)))
        } else {
            Ok(None)
        }
    }

    fn handle_mapper_next(result: Result<Option<T>, E>) -> StageResult<T, E> {
        let message = result.map_err(|code| DirectionError::StreamMapper(code))?;

        match message {
            Some(it) => Ok(Some(PreparedFuture::SenderSend(Some(it)))),
            None => Ok(None),
        }
    }

    fn handle_sender_send(
        result: Result<(), Outcome<E>>,
        state: &State<T, SD, E>,
    ) -> StageResult<T, E> {
        result.map_err(Self::send_to_direction_err)?;

        if state.decoder.has_message() {
            Ok(Some(PreparedFuture::MapperNext))
        } else {
            Ok(None)
        }
    }


    fn close(
        &mut self,
        error: DirectionError<E>,
        state: State<T, SD, E>,
        stream: &mut ReadStream<BufViewFactory>,
    ) {
        self.open = false;
        let _ = self.executor.cancel_sender().send(error);

        match error {
            DirectionError::Finish => {}

            DirectionError::Internal => {
                let _ = stream.shutdown(E::internal());
            }

            // wire <<< STOP_SENDING(code) <<< our application
            DirectionError::StopSending(code) => {
                let _ = stream.shutdown(code);
            }

            // wire >>> RESET_STREAM(code) >>> our application
            DirectionError::ResetStream(code) => {
                state.sender.terminate(code);
            }

            // wire >>> bytes >>> our application >>> StreamReader error
            DirectionError::StreamMapper(code) => {
                state.sender.terminate_codec(code);
                let _ = stream.shutdown(code);
            }
        }
    }

    #[rustfmt::skip]
    fn send_to_direction_err(error: Outcome<E>) -> DirectionError<E> {
        todo!()
    }
}


type StageResult<T, E> = Result<Option<PreparedFuture<T>>, DirectionError<E>>;

struct State<T, SD, E> {
    decoder: SD,
    sender: Sender<T, E>,
    finish: bool,
}

enum PreparedFuture<T> {
    MapperNotify(usize),
    MapperNext,
    SenderSend(Option<T>),
}

enum FutureResult<T, E> {
    MapperNotify(Result<(), E>),
    MapperNext(Result<Option<T>, E>),
    SenderSend(Result<(), Outcome<E>>),
}

impl<T, SD, E> exec::PreparedFuture<State<T, SD, E>, FutureResult<T, E>> for PreparedFuture<T>
where
    T: Send,
    E: ApplicationError,
    SD: StreamDecoder<T, E> + Send,
{
    async fn run(self, state: &mut State<T, SD, E>) -> FutureResult<T, E> {
        match self {
            PreparedFuture::MapperNotify(length) => {
                FutureResult::MapperNotify(state.decoder.notify_read(length, state.finish).await)
            }

            PreparedFuture::MapperNext => {
                FutureResult::MapperNext(state.decoder.next_message().await)
            }

            PreparedFuture::SenderSend(message) => {
                let finish = state.finish && !state.decoder.has_message();

                let result = match (message, finish) {
                    (Some(message), false) => state.sender.send(message).await,
                    (Some(message), true) => todo!(),
                    (None, true) => todo!(),
                    (None, false) => panic!("cannot send (message:None, finish:false)"),
                };

                FutureResult::SenderSend(result)
            }
        }
    }
}
