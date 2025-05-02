use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

#[derive(Debug, Default)]
struct State {
    paused: bool,
    waker: Option<Waker>,
}


#[derive(Debug, Clone)]
struct SharedState {
    state: Arc<Mutex<State>>,
}

impl SharedState {
    pub fn new(state: Arc<Mutex<State>>) -> Self {
        Self { state }
    }

    pub fn lock(&self) -> MutexGuard<State> {
        self.state.lock().unwrap_or_else(|poison| {
            let mut guard = poison.into_inner();
            Self::resume(&mut guard);

            self.state.clear_poison();
            guard
        })
    }


    pub fn pause(guard: &mut MutexGuard<State>) {
        guard.paused = true;
    }

    pub fn resume(guard: &mut MutexGuard<State>) {
        guard.paused = false;

        if let Some(waker) = mem::replace(&mut guard.waker, None) {
            waker.wake();
        }
    }
}


#[derive(Debug)]
pub struct SuspensionController {
    state: SharedState,
}

impl SuspensionController {
    fn new(state: SharedState) -> Self {
        Self { state }
    }

    pub fn pause(&self) {
        SharedState::pause(&mut self.state.lock());
    }

    pub fn resume(&self) {
        SharedState::resume(&mut self.state.lock());
    }
}

impl Drop for SuspensionController {
    fn drop(&mut self) {
        self.resume();
    }
}


#[derive(Debug)]
pub struct SuspendableFuture<F: Future + Unpin> {
    inner: F,
    state: SharedState,
}

impl<F: Future + Unpin> SuspendableFuture<F> {
    pub fn new(inner: F) -> (Self, SuspensionController) {
        let state = SharedState::new(Arc::new(Mutex::new(State::default())));

        (
            Self {
                inner,
                state: state.clone(),
            },
            SuspensionController::new(state),
        )
    }
}

impl<F: Future + Unpin> Future for SuspendableFuture<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        {
            let mut guard = self.state.lock();

            if guard.paused {
                guard.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }

        Pin::new(&mut self.inner).poll(cx)
    }
}
