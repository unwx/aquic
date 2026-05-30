// TODO(test): untested unsafe code.

use crate::debug_panic;
use crate::sync::mpsc::rpc::{RemoteCall, RemoteCallback, RemoteClient, SendError};
use crate::sync::mpsc::unbounded;
use crate::sync::mpsc::unbounded::UnboundedSender;
use futures::task::AtomicWaker;
use std::cell::UnsafeCell;
use std::fmt::{Debug, Formatter};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::task::{Context, Poll};


type Slab<R> = sharded_slab::Slab<Slot<R>>;

struct Slot<R> {
    state: AtomicU8,
    waker: AtomicWaker,
    result: UnsafeCell<Option<R>>,
}

// SAFETY: We guarantee strict memory access using `Slot.state`.
unsafe impl<R: Send> Send for Slot<R> {}
unsafe impl<R: Send> Sync for Slot<R> {}


/// A response/callback state.
///
/// State may only be transitioned by `Callback`,
/// while `ResponseFuture` only reads it.
#[repr(u8)]
#[derive(Debug, Copy, Clone)]
enum State {
    Pending = 0,
    Ready = 1,
    HangUp = 2,
}

impl From<u8> for State {
    fn from(value: u8) -> Self {
        match value {
            x if x == State::Pending as u8 => Self::Pending,
            x if x == State::Ready as u8 => Self::Ready,
            x if x == State::HangUp as u8 => Self::HangUp,
            _ => unreachable!(),
        }
    }
}

impl From<State> for u8 {
    fn from(value: State) -> Self {
        value as u8
    }
}


pub(crate) type Call<T, R> = RemoteCall<T, Callback<R>>;

pub(crate) struct Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    sender: unbounded::Sender<Call<T, R>>,
    storage: Arc<Slab<R>>,
}

impl<T, R> RemoteClient<T, R, Callback<R>> for Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    fn new(sender: unbounded::Sender<Call<T, R>>) -> Self {
        Self {
            sender,
            storage: Arc::new(Slab::new()),
        }
    }

    async fn send(&self, args: T) -> Result<R, SendError> {
        let key = {
            let slot = Slot {
                state: AtomicU8::new(State::Pending.into()),
                waker: AtomicWaker::new(),
                result: UnsafeCell::new(None),
            };

            let Some(key) = self.storage.insert(slot) else {
                debug_panic!("unable to insert an RPC call state: sharded-slab's shard is full");
                return Err(SendError::NoResponse);
            };

            key
        };

        if self
            .sender
            .send(Call {
                args,
                callback: Some(Callback {
                    storage: self.storage.clone(),
                    key,
                }),
            })
            .is_err()
        {
            self.storage.remove(key);
            return Err(SendError::Closed);
        }

        ResponseFuture {
            storage: self.storage.clone(),
            key,
        }
        .await
    }

    fn send_and_forget(&self, args: T) -> Result<(), SendError> {
        match self.sender.send(Call {
            args,
            callback: None,
        }) {
            Ok(_) => Ok(()),
            Err(_) => Err(SendError::Closed),
        }
    }
}

impl<T, R> Debug for Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("mt::Remote").finish()
    }
}

impl<T, R> Clone for Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            storage: self.storage.clone(),
        }
    }
}


pub(crate) struct Callback<R> {
    storage: Arc<Slab<R>>,
    key: usize,
}

impl<R> RemoteCallback<R> for Callback<R> {
    fn on_result(self, result: R) {
        let Some(entry) = self.storage.get(self.key) else {
            return;
        };

        // SAFETY:
        // - There is no race condition on this method, as we have exclusive access to `self`.
        // - The `ResponseFuture` won't read the result until we set the `state` to `Ready`.
        // - The state is set with `Release` ordering, result must become visible afterwards.
        unsafe {
            *entry.result.get() = Some(result);
        }

        // SAFETY (why not `Pending` -> `Ready` CAS):
        // - `self` is consumed, meaning `on_result` can only be called once.
        // - `Drop` cannot run concurrently.
        // - We exclusively own the right to transition out of the `Pending` state.
        entry.state.store(State::Ready.into(), Release);

        entry.waker.wake();
    }
}

impl<R> Drop for Callback<R> {
    fn drop(&mut self) {
        let Some(entry) = self.storage.get(self.key) else {
            return;
        };

        if entry
            .state
            .compare_exchange(
                State::Pending.into(),
                State::HangUp.into(),
                Release,
                Relaxed,
            )
            .is_ok()
        {
            entry.waker.wake();
        }
    }
}


pub(crate) struct ResponseFuture<R> {
    storage: Arc<Slab<R>>,
    key: usize,
}

impl<R> Future for ResponseFuture<R> {
    type Output = Result<R, SendError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(entry) = self.storage.get(self.key) else {
            return Poll::Ready(Err(SendError::NoResponse));
        };

        let mut state = State::from(entry.state.load(Acquire));

        if matches!(state, State::Pending) {
            entry.waker.register(cx.waker());

            // Double-check the `state` after registering the waker,
            // to prevent race conditions where the callback writes & wakes between the first check and registration.
            state = State::from(entry.state.load(Acquire));
        }

        match state {
            State::Pending => Poll::Pending,
            State::HangUp => Poll::Ready(Err(SendError::NoResponse)),
            State::Ready => {
                // SAFETY:
                // - State is `Ready`, therefore result is guaranteed to be set.
                // - We use `Acquire/Release` memory ordering.
                // - `poll()` accepts a mutable reference to Self, meaning that `poll()` is sequential.
                let result = unsafe { (*entry.result.get()).take() };
                Poll::Ready(result.ok_or(SendError::NoResponse))
            }
        }
    }
}

impl<R> Drop for ResponseFuture<R> {
    fn drop(&mut self) {
        // SAFETY: See `sharded_slab::Entry` doc.
        // >
        // While the guard exists, it indicates to the slab that the item the guard
        // references is currently being accessed. If the item is removed from the slab
        // while a guard exists, the removal will be deferred until all guards are
        // dropped.
        self.storage.remove(self.key);
    }
}
