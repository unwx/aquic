use crate::sync::mpsc::unbounded::{UnboundedReceiver, UnboundedSender, st};
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender, has_capacity};
use crate::sync::mpsc::{SendError, TryRecvError};
use event_listener::Event;
use std::cell::RefCell;
use std::rc::Rc;

#[rustfmt::skip]
pub(crate) fn channel<T: Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = st::channel();
    let shared = Rc::new(RefCell::new(Shared {
        event: Event::new(),
        occupation: 0,
        bound: bound.max(1),
        open: true,
    }));

    (
        Sender { sender, shared: shared.clone() },
        Receiver { receiver, shared },
    )
}


pub(crate) struct Sender<T> {
    sender: st::Sender<(T, usize)>,
    shared: Rc<RefCell<Shared>>,
}

impl<T: Unpin + 'static> WeightedSender<T> for Sender<T> {
    async fn send(&self, value: T, weight: usize) -> Result<(), SendError> {
        loop {
            let mut shared = self.shared.borrow_mut();

            if !shared.open {
                return Err(SendError);
            }
            if has_capacity(shared.occupation, weight, shared.bound) {
                shared.occupation += weight;
                break;
            }

            let listener = shared.event.listen();
            drop(shared);

            listener.await;
        }

        if let Err(_) = self.sender.send((value, weight)) {
            let mut shared = self.shared.borrow_mut();
            shared.open = false;
            shared.occupation = 0;
            shared.event.notify(usize::MAX);
            return Err(SendError);
        }

        Ok(())
    }

    fn occupation(&self) -> usize {
        self.shared.borrow().occupation
    }

    fn is_closed(&self) -> bool {
        !self.shared.borrow().open
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
    receiver: st::Receiver<(T, usize)>,
    shared: Rc<RefCell<Shared>>,
}

impl<T: Unpin + 'static> WeightedReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
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
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.open = false;
        shared.occupation = 0;
        shared.event.notify(usize::MAX);
    }
}


struct Shared {
    // TODO(perf): replace with a not thread-safe intrusive list?
    pub event: Event,
    pub occupation: usize,
    pub bound: usize,
    pub open: bool,
}
