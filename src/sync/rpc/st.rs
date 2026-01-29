use crate::sync::mpsc;
use crate::sync::mpsc::unbounded::UnboundedSender;
use crate::sync::rpc::{RemoteCall, RemoteCallback, RemoteClient, SendError};
use slab::Slab;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

pub(crate) type Call<T, R> = RemoteCall<T, Callback<R>>;

enum State<R> {
    Pending(Option<Waker>),
    Ready(R),
    HangUp,
}


struct Storage<R> {
    pub slab: Slab<(State<R>, u32)>,
    pub seq: u32,
}

impl<R> Storage<R> {
    pub fn next_version(&mut self) -> u32 {
        let version = self.seq;
        self.seq += 1;
        version
    }
}

impl<R> Deref for Storage<R> {
    type Target = Slab<(State<R>, u32)>;

    fn deref(&self) -> &Self::Target {
        &self.slab
    }
}

impl<R> DerefMut for Storage<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.slab
    }
}


pub(crate) struct Remote<T, R> {
    sender: mpsc::unbounded::Sender<Call<T, R>>,
    storage: Rc<RefCell<Storage<R>>>,
}

impl<T, R> RemoteClient<T, R, Callback<R>> for Remote<T, R>
where
    T: Unpin + 'static,
    R: Unpin + 'static,
{
    fn new(sender: mpsc::unbounded::Sender<Call<T, R>>) -> Self {
        Self {
            sender,
            storage: Rc::new(RefCell::new(Storage {
                slab: Slab::with_capacity(2),
                seq: 0,
            })),
        }
    }

    async fn send(&self, args: T) -> Result<R, SendError> {
        let mut storage = self.storage.borrow_mut();
        let version = storage.next_version();
        let key = storage.insert((State::Pending(None), version));

        if let Err(_) = self.sender.send(Call {
            args,
            callback: Some(Callback {
                storage: self.storage.clone(),
                key,
                version,
            }),
        }) {
            storage.remove(key);
            return async { Err(SendError::Closed) };
        }

        drop(storage);
        ResponseFuture {
            storage: self.storage.clone(),
            key,
            version,
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

impl<T, R> Debug for crate::sync::rpc::mt::Remote<T, R> {
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
    storage: Rc<RefCell<Storage<R>>>,
    key: usize,
    version: u32,
}

impl<R> RemoteCallback<R> for Callback<R> {
    //noinspection DuplicatedCode
    fn on_result(self, result: R) {
        let Some((state, version)) = self.storage.borrow_mut().get_mut(self.key) else {
            return;
        };
        if self.version != *version {
            return;
        }

        match state {
            State::Pending(it) => {
                let waker = it.clone();
                *state = State::Ready(result);

                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            State::Ready(_) | State::HangUp => {
                if cfg!(debug_assertions) {
                    panic!(
                        "bug: unable to make a result callback for [key: {}, version: {}]: \
                        result is already present",
                        self.key, self.version
                    );
                }
            }
        };
    }
}

impl<R> Drop for Callback<R> {
    //noinspection DuplicatedCode
    fn drop(&mut self) {
        let Ok(mut storage) = self.storage.try_borrow_mut() else {
            return;
        };
        let Some((state, version)) = storage.get_mut(self.key) else {
            return;
        };
        if self.version != *version {
            return;
        }

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


pub struct ResponseFuture<R> {
    storage: Rc<RefCell<Storage<R>>>,
    key: usize,
    version: u32,
}

impl<R> Future for ResponseFuture<R> {
    type Output = Result<R, SendError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slab = self.storage.borrow_mut();

        let Some((state, version)) = slab.get_mut(self.key) else {
            return Poll::Ready(Err(SendError::NoResponse));
        };
        if self.version != *version {
            return Poll::Ready(Err(SendError::NoResponse));
        }

        match state {
            State::Pending(_) => {
                *state = State::Pending(Some(cx.waker().clone()));
                Poll::Pending
            }
            State::Ready(_) => {
                if let State::Ready(val) = slab.remove(self.key) {
                    return Poll::Ready(Ok(val));
                }

                panic!(
                    "bug: race condition in single-threaded RPC channel: State is no more Ready"
                );
            }
            State::HangUp => Poll::Ready(Err(SendError::NoResponse)),
        }
    }
}

impl<R> Drop for ResponseFuture<R> {
    fn drop(&mut self) {
        let Ok(mut slab) = self.storage.try_borrow_mut() else {
            return;
        };
        let Some((_, version)) = slab.get_mut(self.key) else {
            return;
        };

        if self.version == *version {
            slab.remove(self.key);
        }
    }
}
