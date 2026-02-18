use crate::sync::mpsc::unbounded::{self, UnboundedReceiver, UnboundedSender};
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender, has_capacity};
use crate::sync::{SendError, TryRecvError, TrySendError};
use event_listener::Event;
use std::cell::RefCell;
use std::rc::Rc;

#[rustfmt::skip]
pub(crate) fn channel<T: Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = unbounded::channel();
    let shared = Rc::new(RefCell::new(Shared {
        event: Event::new(),
        occupation: 0,
        bound: bound.max(1),
    }));

    (
        Sender { sender, shared: shared.clone() },
        Receiver { receiver, shared },
    )
}


pub(crate) struct Sender<T> {
    sender: unbounded::Sender<(T, usize)>,
    shared: Rc<RefCell<Shared>>,
}

impl<T: Unpin + 'static> WeightedSender<T> for Sender<T> {
    async fn send(&self, value: T, weight: usize) -> Result<(), SendError<T>> {
        loop {
            let mut shared = self.shared.borrow_mut();

            if self.sender.is_closed() {
                return Err(SendError(value));
            }
            if has_capacity(shared.occupation, weight, shared.bound) {
                shared.occupation += weight;
                break;
            }

            let listener = shared.event.listen();
            drop(shared);

            // Cancel safe: we haven't modified anything yet.
            listener.await;
        }

        if let Err(e) = self.sender.send((value, weight)) {
            let mut shared = self.shared.borrow_mut();

            // No receiver exists, we can safely store just `0`.
            shared.occupation = 0;
            shared.event.notify(usize::MAX);
            return Err(SendError(e.0.0));
        }

        Ok(())
    }

    fn try_send(&self, value: T, weight: usize) -> Result<(), TrySendError> {
        let mut shared = self.shared.borrow_mut();

        if self.sender.is_closed() {
            return Err(TrySendError::Closed);
        }
        if !has_capacity(shared.occupation, weight, shared.bound) {
            return Err(TrySendError::Full);
        }

        shared.occupation += weight;
        if self.sender.send((value, weight)).is_err() {
            // No receiver exists, we can safely store just `0`.
            shared.occupation = 0;
            shared.event.notify(usize::MAX);
            return Err(TrySendError::Closed);
        }

        Ok(())
    }

    fn occupation(&self) -> usize {
        self.shared.borrow().occupation
    }

    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            shared: self.shared.clone(),
        }
    }
}


pub(crate) struct Receiver<T> {
    receiver: unbounded::Receiver<(T, usize)>,
    shared: Rc<RefCell<Shared>>,
}

impl<T: Unpin + 'static> WeightedReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        // Cancel safe: mpsc::Receiver::recv() is cancel safe.
        let (value, weight) = self.receiver.recv().await?;

        let mut shared = self.shared.borrow_mut();
        shared.occupation -= weight;

        if has_capacity(shared.occupation, weight, shared.bound) {
            shared.event.notify(usize::MAX);
        }

        Some(value)
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let (value, weight) = self.receiver.try_recv()?;

        let mut shared = self.shared.borrow_mut();
        shared.occupation -= weight;

        if has_capacity(shared.occupation, weight, shared.bound) {
            shared.event.notify(usize::MAX);
        }

        Ok(value)
    }

    fn occupation(&self) -> usize {
        self.shared.borrow().occupation
    }

    fn is_closed(&self) -> bool {
        self.receiver.is_closed()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.occupation = 0;
        shared.event.notify(usize::MAX);
    }
}


struct Shared {
    // TODO(perf): replace with a not thread-safe intrusive list?
    pub event: Event,
    pub occupation: usize,
    pub bound: usize,
}
