use event_listener::Event;

use std::cell::RefCell;
use std::rc::Rc;
use std::usize;

use crate::sync::mpmc::watch::{WatchReceiver, WatchSender};
use crate::sync::{SendError, TryRecvError};

const INITIAL_SENDER_VERSION: usize = 0;
const INITIAL_RECEIVER_VERSION: usize = usize::MAX;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        item: None,
        event: Event::new(),
        sender_count: 1,
        receiver_count: 1,
    }));

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
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T> WatchSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut shared = self.shared.borrow_mut();

        if shared.receiver_count == 0 {
            return Err(SendError(value));
        }

        let next_version = {
            if let Some(item) = shared.item.as_ref() {
                item.version + 1
            } else {
                INITIAL_SENDER_VERSION
            }
        };

        shared.item.replace(Item {
            value,
            version: next_version,
        });
        shared.event.notify(usize::MAX);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.borrow().receiver_count == 0
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.borrow_mut().sender_count += 1;

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.sender_count -= 1;

        if shared.sender_count == 0 {
            shared.event.notify(usize::MAX);
        }
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Rc<RefCell<Shared<T>>>,
    last_seen_version: usize,
}

impl<T: Clone> WatchReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        loop {
            match self.try_recv() {
                Ok(value) => {
                    return Some(value);
                }
                Err(TryRecvError::Closed) => {
                    return None;
                }
                Err(TryRecvError::Empty) => self.shared.borrow().event.listen().await,
            }
        }
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.try_recv_inner(false)
    }

    fn try_recv_any(&mut self) -> Result<T, crate::sync::TryRecvError> {
        self.try_recv_inner(true)
    }

    fn is_closed(&self) -> bool {
        self.shared.borrow().sender_count == 0
    }
}

impl<T: Clone> Receiver<T> {
    fn try_recv_inner(&mut self, ignore_version: bool) -> Result<T, TryRecvError> {
        let shared = self.shared.borrow();

        if let Some(item) = shared.item.as_ref() {
            if ignore_version || self.last_seen_version != item.version {
                self.last_seen_version = item.version;
                return Ok(item.value.clone());
            }
        }

        if shared.sender_count == 0 {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.borrow_mut().receiver_count += 1;

        Self {
            shared: self.shared.clone(),
            last_seen_version: self.last_seen_version.clone(),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.borrow_mut().receiver_count -= 1;
    }
}


#[derive(Debug)]
struct Shared<T> {
    pub item: Option<Item<T>>,
    pub event: Event, // TODO(perf): replace with a not thread-safe intrusive list?
    pub sender_count: usize,
    pub receiver_count: usize,
}

#[derive(Debug)]
struct Item<T> {
    pub value: T,
    pub version: usize,
}
