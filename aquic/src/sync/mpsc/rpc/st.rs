use crate::sync::mpsc;
use crate::sync::mpsc::rpc::{RemoteCall, RemoteCallback, RemoteClient, SendError};
use crate::sync::mpsc::unbounded;
use crate::sync::mpsc::unbounded::UnboundedSender;
use slotmap::{SlotMap, new_key_type};
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};


/// A response/callback state.
///
/// State may only be transitioned by `Callback`,
/// while `ResponseFuture` only reads it.
enum State<R> {
    Pending(Option<Waker>),
    Ready(R),
    HangUp,
}

new_key_type! {
    struct StateKey;
}


pub(crate) type Call<T, R> = RemoteCall<T, Callback<R>>;

pub(crate) struct Remote<T, R> {
    sender: unbounded::Sender<Call<T, R>>,
    storage: Rc<RefCell<SlotMap<StateKey, State<R>>>>,
}

impl<T, R> RemoteClient<T, R, Callback<R>> for Remote<T, R>
where
    T: Unpin + 'static,
    R: Unpin + 'static,
{
    fn new(sender: mpsc::unbounded::Sender<Call<T, R>>) -> Self {
        Self {
            sender,
            storage: Rc::new(RefCell::new(SlotMap::with_key())),
        }
    }

    async fn send(&self, args: T) -> Result<R, SendError> {
        let mut storage = self.storage.borrow_mut();
        let key = storage.insert(State::Pending(None));

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
            storage.remove(key);
            return Err(SendError::Closed);
        }

        ResponseFuture {
            storage: self.storage.clone(),
            key,
        }
        .await
    }

    fn send_and_forget(&self, args: T) -> Result<(), SendError> {
        self.sender
            .send(Call {
                args,
                callback: None,
            })
            .map_err(|_| SendError::Closed)
    }
}

impl<T, R> Debug for Remote<T, R> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("st::Remote").finish()
    }
}

impl<T, R> Clone for Remote<T, R> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            storage: self.storage.clone(),
        }
    }
}


pub(crate) struct Callback<R> {
    storage: Rc<RefCell<SlotMap<StateKey, State<R>>>>,
    key: StateKey,
}

impl<R> RemoteCallback<R> for Callback<R> {
    fn on_result(self, result: R) {
        let mut storage = self.storage.borrow_mut();

        let Some(state) = storage.get_mut(self.key) else {
            return;
        };

        if let State::Pending(waker) = state {
            let waker = waker.take();
            *state = State::Ready(result);

            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}

impl<R> Drop for Callback<R> {
    fn drop(&mut self) {
        let Ok(mut storage) = self.storage.try_borrow_mut() else {
            return;
        };
        let Some(state) = storage.get_mut(self.key) else {
            return;
        };

        if let State::Pending(waker) = state {
            let waker = waker.take();
            *state = State::HangUp;

            if let Some(waker) = waker {
                waker.wake();
            }
        }
    }
}


pub(crate) struct ResponseFuture<R> {
    storage: Rc<RefCell<SlotMap<StateKey, State<R>>>>,
    key: StateKey,
}

impl<R> Future for ResponseFuture<R> {
    type Output = Result<R, SendError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut storage = self.storage.borrow_mut();

        let Some(state) = storage.get_mut(self.key) else {
            return Poll::Ready(Err(SendError::NoResponse));
        };

        match state {
            State::Pending(waker) => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            State::Ready(_) => {
                if let Some(State::Ready(val)) = storage.remove(self.key) {
                    Poll::Ready(Ok(val))
                } else {
                    Poll::Ready(Err(SendError::NoResponse))
                }
            }
            State::HangUp => Poll::Ready(Err(SendError::NoResponse)),
        }
    }
}

impl<R> Drop for ResponseFuture<R> {
    fn drop(&mut self) {
        if let Ok(mut storage) = self.storage.try_borrow_mut() {
            storage.remove(self.key);
        }
    }
}
