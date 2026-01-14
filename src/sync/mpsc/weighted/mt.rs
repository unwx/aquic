use crate::sync::mpsc::unbounded::{UnboundedReceiver, UnboundedSender, mt};
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender, has_capacity};
use crate::sync::mpsc::{SendError, TryRecvError};
use event_listener::Event;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[rustfmt::skip]
pub(crate) fn channel<T: Send + Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = mt::channel();
    let shared = Arc::new(Shared {
        event: Event::new(),
        occupation: AtomicUsize::new(0),
        bound: bound.max(1),
        open: AtomicBool::new(true),
    });

    (
        Sender { sender, shared: shared.clone() },
        Receiver { receiver, shared },
    )
}


pub(crate) struct Sender<T> {
    sender: mt::Sender<(T, usize)>,
    shared: Arc<Shared>,
}

impl<T: Send + Unpin + 'static> WeightedSender<T> for Sender<T> {
    async fn send(&self, value: T, weight: usize) -> Result<(), SendError> {
        let mut current = self.shared.occupation.load(Acquire);

        loop {
            if self.is_closed() {
                return Err(SendError);
            }
            if has_capacity(current, weight, self.shared.bound) {
                match self.shared.occupation.compare_exchange_weak(
                    current,
                    current + weight,
                    Acquire,
                    Relaxed,
                ) {
                    Ok(_) => {
                        break;
                    }
                    Err(new) => {
                        current = new;
                        continue;
                    }
                }
            }

            // Avoid race condition:
            // - Receiver consumed an item (or closed), notified all listeners.
            // - Sender created a listener.
            // - Sender's listener doesn't affected, it will wait for the next signal.

            let listener = self.shared.event.listen();
            if self.is_closed() {
                return Err(SendError);
            }

            current = self.shared.occupation.load(Acquire);
            if has_capacity(current, weight, self.shared.bound) {
                continue;
            }

            listener.await;
            current = self.shared.occupation.load(Acquire);
        }

        if let Err(_) = self.sender.send((value, weight)) {
            self.shared.open.store(false, Release);

            // No receivers left, we can safely store just `0`.
            self.shared.occupation.store(0, Release);
            self.shared.event.notify(usize::MAX);
            return Err(SendError);
        }

        Ok(())
    }

    fn occupation(&self) -> usize {
        self.shared.occupation.load(Acquire)
    }

    fn is_closed(&self) -> bool {
        self.shared.open.load(Acquire)
    }
}

impl<T: Send> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            shared: self.shared.clone(),
        }
    }
}


pub(crate) struct Receiver<T> {
    receiver: mt::Receiver<(T, usize)>,
    shared: Arc<Shared>,
}

impl<T: Send + Unpin + 'static> WeightedReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        let (value, weight) = self.receiver.recv().await?;

        let current = {
            let previous = self.shared.occupation.fetch_sub(weight, Release);
            previous - weight
        };

        if has_capacity(current, weight, self.shared.bound) {
            self.shared.event.notify(usize::MAX);
        }

        Some(value)
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let (value, weight) = self.receiver.try_recv()?;

        let current = {
            let previous = self.shared.occupation.fetch_sub(weight, Release);
            previous - weight
        };

        if has_capacity(current, weight, self.shared.bound) {
            self.shared.event.notify(usize::MAX);
        }

        Ok(value)
    }

    fn occupation(&self) -> usize {
        self.shared.occupation.load(Acquire)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.open.store(false, Release);
        self.shared.occupation.store(0, Release);
        self.shared.event.notify(usize::MAX);
    }
}


struct Shared {
    pub event: Event,
    pub occupation: AtomicUsize,
    pub bound: usize,
    pub open: AtomicBool,
}
