use event_listener::Event;

use std::sync::{Arc, RwLock};
use std::usize;

use crate::sync::mpmc::watch::{WatchReceiver, WatchSender};
use crate::sync::{SendError, TryRecvError};

const INITIAL_SENDER_VERSION: usize = 0;
const INITIAL_RECEIVER_VERSION: usize = usize::MAX;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: RwLock::new(State {
            item: None,
            sender_count: 1,
            receiver_count: 1,
        }),
        event: Event::new(),
    });

    let sender = Sender {
        shared: shared.clone(),
    };
    let receiver = Receiver {
        shared,
        last_seen_version: INITIAL_RECEIVER_VERSION,
    };

    (sender, receiver)
}


#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> WatchSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        {
            let mut state = self.shared.state.write().unwrap();
            if state.receiver_count == 0 {
                return Err(SendError(value));
            }

            let next_version = {
                if let Some(item) = state.item.as_ref() {
                    item.version + 1
                } else {
                    INITIAL_SENDER_VERSION
                }
            };

            state.item.replace(Item {
                value,
                version: next_version,
            });
        }

        self.shared.event.notify(usize::MAX);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.state.read().unwrap().receiver_count == 0
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let current_count = {
            let mut state = self.shared.state.write().unwrap();
            state.sender_count -= 1;
            state.sender_count
        };

        if current_count == 0 {
            self.shared.event.notify(usize::MAX);
        }
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    last_seen_version: usize,
}

impl<T: Clone + Send + Sync> WatchReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        loop {
            match self.try_recv_inner(false) {
                Ok(value) => {
                    return Some(value);
                }
                Err(TryRecvError::Closed) => {
                    return None;
                }
                Err(TryRecvError::Empty) => {}
            }

            let listener = self.shared.event.listen();

            match self.try_recv_inner(false) {
                Ok(value) => {
                    return Some(value);
                }
                Err(TryRecvError::Closed) => {
                    return None;
                }
                Err(TryRecvError::Empty) => {}
            }

            listener.await;
        }
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.try_recv_inner(false)
    }

    fn try_recv_any(&mut self) -> Result<T, TryRecvError> {
        self.try_recv_inner(true)
    }

    fn is_closed(&self) -> bool {
        self.shared.state.read().unwrap().sender_count == 0
    }
}

impl<T: Clone> Receiver<T> {
    fn try_recv_inner(&mut self, ignore_version: bool) -> Result<T, TryRecvError> {
        let state = self.shared.state.read().unwrap();

        if let Some(item) = state.item.as_ref() {
            if ignore_version || self.last_seen_version != item.version {
                self.last_seen_version = item.version;
                return Ok(item.value.clone());
            }
        }

        if state.receiver_count == 0 {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.state.write().unwrap().receiver_count += 1;

        Self {
            shared: self.shared.clone(),
            last_seen_version: self.last_seen_version,
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.state.write().unwrap().receiver_count -= 1;
    }
}


#[derive(Debug)]
struct Shared<T> {
    pub state: RwLock<State<T>>,
    pub event: Event,
}

#[derive(Debug, Clone)]
struct State<T> {
    pub item: Option<Item<T>>,
    pub sender_count: usize,
    pub receiver_count: usize,
}

#[derive(Debug, Clone)]
struct Item<T> {
    pub value: T,
    pub version: usize,
}
