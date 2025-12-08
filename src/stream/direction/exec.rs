use crate::ApplicationError;
use crate::stream::direction::DirectionError;
use crate::sync::oneshot;
use futures::FutureExt;
use std::cell::UnsafeCell;
use tokio::select;
use tokio::sync::mpsc;
use tracing::debug;

pub trait PreparedFuture<S, R> {
    fn run(self, state: &mut S) -> impl Future<Output = R> + Send;
}

pub trait CompletedFuture<S, R, E> {
    fn from(state: S, result: Result<R, DirectionError<E>>) -> Self;

    fn into_tuple(self) -> (S, Result<R, DirectionError<E>>);
}

pub enum OrchestrateResult<S, E> {
    Complete(S),
    Schedule,
    Error(S, DirectionError<E>),
}


pub struct Executor<CF, E> {
    stream_id: u64,
    completed_future_sender: mpsc::UnboundedSender<(u64, CF)>,
    cancel_sender: oneshot::Sender<DirectionError<E>>,
    cancel_receiver: oneshot::Receiver<DirectionError<E>>,
}

impl<CF, E> Executor<CF, E> {
    pub fn new(stream_id: u64, completed_future_sender: mpsc::UnboundedSender<(u64, CF)>) -> Self {
        let cancel_channel = oneshot::channel();

        Self {
            stream_id,
            completed_future_sender,
            cancel_sender: cancel_channel.0,
            cancel_receiver: cancel_channel.1,
        }
    }

    pub fn cancel_sender(&self) -> oneshot::Sender<DirectionError<E>> {
        self.cancel_sender.clone()
    }

    pub fn cancel_receiver(&self) -> oneshot::Receiver<DirectionError<E>> {
        self.cancel_receiver.clone()
    }
}

impl<CF, E> Clone for Executor<CF, E> {
    fn clone(&self) -> Self {
        Self {
            stream_id: self.stream_id,
            completed_future_sender: self.completed_future_sender.clone(),
            cancel_sender: self.cancel_sender.clone(),
            cancel_receiver: self.cancel_receiver.clone(),
        }
    }
}


pub fn orchestrate_futures<S, R, PF, CF, HF, E>(
    mut state: S,
    mut future_result: Result<R, DirectionError<E>>,
    executor: &Executor<CF, E>,
    mut handler_fn: HF,
) -> OrchestrateResult<S, E>
where
    E: ApplicationError,
    S: Send + 'static,
    R: 'static,
    PF: PreparedFuture<S, R> + Send + 'static,
    CF: CompletedFuture<S, R, E> + Send + 'static,
    HF: FnMut(R, &mut S) -> Result<Option<PF>, DirectionError<E>>,
{
    loop {
        let result = match future_result {
            Ok(it) => it,
            Err(e) => {
                return OrchestrateResult::Error(state, e);
            }
        };

        return match handler_fn(result, &mut state) {
            Ok(Some(prepared_future)) => {
                if let Some(completed_future) = try_complete_now(state, prepared_future, executor) {
                    (state, future_result) = completed_future.into_tuple();
                    continue;
                }

                OrchestrateResult::Schedule
            }
            Ok(None) => OrchestrateResult::Complete(state),
            Err(e) => OrchestrateResult::Error(state, e),
        };
    }
}

pub fn try_complete_now<S, R, PF, CF, E>(
    state: S,
    prepared_future: PF,
    executor: &Executor<CF, E>,
) -> Option<CF>
where
    E: ApplicationError,
    R: 'static,
    S: Send + 'static,
    PF: PreparedFuture<S, R> + Send + 'static,
    CF: CompletedFuture<S, R, E> + Send + 'static,
{
    // TODO(safety): is there a way to rewrite this safely?
    //  Without making PreparedFuture::run() creepy...

    let state = UnsafeCell::new(state);
    let mut future = {
        // SAFETY: state is initialized and is used only after the future is dropped.
        let state_ref = unsafe { &mut *state.get() };
        Box::pin(prepared_future.run(state_ref))
    };

    // TODO(performance): bench this, is it impactful or not.
    if let Some(result) = future.as_mut().now_or_never() {
        drop(future);
        return Some(CF::from(state.into_inner(), Ok(result)));
    }

    let executor = executor.clone();
    tokio::spawn(async move {
        let result = exec_cancellable(future, executor.cancel_receiver.clone()).await;

        // `future` is dropped.
        let state = state.into_inner();

        if executor
            .completed_future_sender
            .send((executor.stream_id, CF::from(state, result)))
            .is_err()
        {
            debug!("detected 'completed_future' channel hang-up");
            let _ = executor.cancel_sender.send(DirectionError::Internal);
        };
    });

    None
}

pub async fn exec_cancellable<T, F, E>(
    future: F,
    cancel_receiver: oneshot::Receiver<DirectionError<E>>,
) -> Result<T, DirectionError<E>>
where
    E: Clone,
    F: Future<Output = T>,
{
    select! {
        biased;

        error = cancel_receiver.recv() => {
            Err(error.unwrap_or_else(|| {
                debug!("detected 'cancel_receiver' channel hang-up");
                DirectionError::Internal
            }))
        },
        result = future => Ok(result)
    }
}
