use event_listener::Event;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::{Arc, OnceLock};
use std::usize;

use crate::sync::TryRecvError;
use crate::sync::mpmc::oneshot::{OneshotReceiver, OneshotSender, SendError};

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        container: OnceLock::new(),
        event: Event::new(),
        sender_count: AtomicUsize::new(1),
        receiver_count: AtomicUsize::new(1),
    });

    #[rustfmt::skip]
    let sender = Sender { shared: shared.clone() };
    let receiver = Receiver { shared };

    (sender, receiver)
}


#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone> OneshotSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        if self.is_closed() {
            return Err(SendError::Closed);
        }

        {
            // Since `try_insert()` is unstable:
            let mut value = Some(value);
            let result = self.shared.container.get_or_init(|| value.take().unwrap());

            if value.is_some() {
                return Err(SendError::Full(result.clone()));
            }
        }

        self.shared.event.notify(usize::MAX);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.receiver_count.load(Acquire) == 0
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Relaxed);

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let current_count = self.shared.sender_count.fetch_sub(1, Release) - 1;

        if current_count == 0 {
            self.shared.event.notify(usize::MAX);
        }
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone + Send + Sync> OneshotReceiver<T> for Receiver<T> {
    async fn recv(&self) -> Option<T> {
        loop {
            match self.try_recv() {
                Ok(value) => {
                    return Some(value);
                }
                Err(TryRecvError::Closed) => {
                    return None;
                }
                Err(TryRecvError::Empty) => {}
            }

            let listener = self.shared.event.listen();

            match self.try_recv() {
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

    fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(value) = self.shared.container.get() {
            return Ok(value.clone());
        }
        if self.is_closed() {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }

    fn is_closed(&self) -> bool {
        self.shared.sender_count.load(Acquire) == 0
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.receiver_count.fetch_add(1, Relaxed);

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.receiver_count.fetch_sub(1, Release);
    }
}


#[derive(Debug)]
struct Shared<T> {
    pub container: OnceLock<T>,
    pub event: Event,
    pub sender_count: AtomicUsize,
    pub receiver_count: AtomicUsize,
}
