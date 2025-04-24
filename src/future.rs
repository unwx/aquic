use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

#[derive(Debug, Default)]
struct State {
    paused: bool,
    waker: Option<Waker>,
}


#[derive(Debug, Clone)]
pub struct SuspensionController {
    state: Arc<Mutex<State>>,
}

impl SuspensionController {
    fn new(state: Arc<Mutex<State>>) -> Self {
        Self { state }
    }

    pub fn pause(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.paused = true;
    }

    pub fn resume(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.paused = false;

        if let Some(waker) = mem::replace(&mut guard.waker, None) {
            waker.wake();
        }
    }
}


#[derive(Debug)]
pub struct SuspendableFuture<F: Future + Unpin> {
    inner: F,
    state: Arc<Mutex<State>>,
}

impl<F: Future + Unpin> SuspendableFuture<F> {
    pub fn new(inner: F) -> (Self, SuspensionController) {
        let state = Arc::new(Mutex::new(State::default()));

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
            let mut guard = self.state.lock().unwrap();

            if guard.paused {
                guard.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }

        Pin::new(&mut self.inner).poll(cx)
    }
}
