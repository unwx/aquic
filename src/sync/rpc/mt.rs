use crate::sync::mpsc;
use crate::sync::mpsc::unbounded::UnboundedSender;
use crate::sync::rpc::{RemoteCall, RemoteCallback, RemoteClient, SendError};
use std::fmt::{Debug, Formatter};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

pub(crate) type Call<T, R> = RemoteCall<T, Callback<R>>;

type Slab<R> = sharded_slab::Slab<Mutex<Option<State<R>>>, SlabConfig>;


struct SlabConfig;

impl sharded_slab::Config for SlabConfig {
    const INITIAL_PAGE_SIZE: usize = 2;
}


enum State<R> {
    Pending(Option<Waker>),
    Ready(R),
    HangUp,
}


pub(crate) struct Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    sender: mpsc::unbounded::Sender<Call<T, R>>,

    // According to `shared_slab` doc, each returned key on `insert` has a generation included,
    // therefore we don't need to track generations ourselves.
    storage: Arc<Slab<R>>,
}

impl<T, R> RemoteClient<T, R, Callback<R>> for Remote<T, R>
where
    T: Send + Unpin + 'static,
    R: Send + Unpin + 'static,
{
    fn new(sender: mpsc::unbounded::Sender<Call<T, R>>) -> Self {
        Self {
            sender,
            storage: Arc::new(sharded_slab::Slab::new_with_config::<SlabConfig>()),
        }
    }

    async fn send(&self, args: T) -> Result<R, SendError> {
        let key = self
            .storage
            .insert(Mutex::new(Some(State::Pending(None))))
            .expect("unable to insert an RPC call state: sharded-slab's shard is full");

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
    //noinspection DuplicatedCode
    fn on_result(self, result: R) {
        let Some(entry) = self.storage.get(self.key) else {
            return;
        };

        let mut guard = entry.lock().unwrap();
        let Some(state) = guard.as_mut() else {
            return;
        };

        match state {
            State::Pending(waker) => {
                let waker = waker.clone();
                *state = State::Ready(result);

                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            State::Ready(_) | State::HangUp => {
                if cfg!(debug_assertions) {
                    panic!(
                        "bug: unable to make a result callback for [key: {}]: \
                        result is already present",
                        self.key
                    );
                }
            }
        };
    }
}

impl<R> Drop for Callback<R> {
    //noinspection DuplicatedCode
    fn drop(&mut self) {
        let Some(entry) = self.storage.get(self.key) else {
            return;
        };

        let Ok(mut guard) = entry.lock() else {
            return;
        };
        let Some(state) = guard.as_mut() else {
            return;
        };

        match state {
            State::Pending(waker) => {
                let waker = waker.clone();
                *state = State::HangUp;

                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            State::Ready(_) | State::HangUp => {}
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

        let mut guard = entry.lock().unwrap();
        let Some(state) = guard.take() else {
            return Poll::Ready(Err(SendError::NoResponse));
        };

        match state {
            State::Pending(_) => {
                *guard = Some(State::Pending(Some(cx.waker().clone())));
                Poll::Pending
            }
            State::HangUp => {
                *guard = Some(State::HangUp);
                Poll::Ready(Err(SendError::NoResponse))
            }
            State::Ready(result) => Poll::Ready(Ok(result)),
        }
    }
}

impl<R> Drop for ResponseFuture<R> {
    fn drop(&mut self) {
        let Some(entry) = self.storage.get(self.key) else {
            return;
        };

        self.storage.remove(self.key);
        drop(entry);
    }
}
