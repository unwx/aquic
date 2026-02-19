use std::cell::RefCell;
use std::rc::Rc;
use std::usize;

use crate::sync::TryRecvError;
use crate::sync::mpmc::oneshot::{OneshotReceiver, OneshotSender, SendError};
use crate::sync::util::event::Event;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        container: None,
        event: Event::new(),
        sender_count: 1,
        receiver_count: 1,
    }));

    #[rustfmt::skip]
    let sender = Sender { shared: shared.clone() };
    let receiver = Receiver { shared };

    (sender, receiver)
}


#[derive(Debug)]
pub struct Sender<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T: Clone> OneshotSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut shared = self.shared.borrow_mut();

        if shared.receiver_count == 0 {
            return Err(SendError::Closed);
        }
        if let Some(current_value) = shared.container.as_ref() {
            return Err(SendError::Full(current_value.clone()));
        }

        shared.container = Some(value);
        shared.event.notify_all();
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
            shared.event.notify_all();
        }
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T: Clone> OneshotReceiver<T> for Receiver<T> {
    async fn recv(&self) -> Option<T> {
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

    fn try_recv(&self) -> Result<T, TryRecvError> {
        let shared = self.shared.borrow();

        if let Some(value) = shared.container.as_ref() {
            return Ok(value.clone());
        }
        if shared.sender_count == 0 {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }

    fn is_closed(&self) -> bool {
        self.shared.borrow().sender_count == 0
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.borrow_mut().receiver_count += 1;

        Self {
            shared: self.shared.clone(),
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
    pub container: Option<T>,
    pub event: Event,
    pub sender_count: usize,
    pub receiver_count: usize,
}
