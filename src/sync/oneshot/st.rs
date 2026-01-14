use crate::sync::oneshot::st::shared::{Notify, Shared};
use crate::sync::oneshot::{OneshotReceiver, OneshotSender, SendError, TryRecvError};
use std::cell::RefCell;
use std::rc::Rc;

#[rustfmt::skip]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        value: None,
        notify: Notify::new(),
        sender_count: 1,
        receiver_count: 1,
    }));

    let sender = Sender { shared: shared.clone() };
    let receiver = Receiver { shared };

    (sender, receiver)
}


#[derive(Debug)]
pub struct Sender<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T> OneshotSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError> {
        let mut shared = self.shared.borrow_mut();

        if shared.is_closed() {
            return Err(SendError::Closed);
        }
        if shared.value.is_some() {
            return Err(SendError::Full);
        }

        shared.value = Some(value);
        shared.notify.notify();
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.borrow().is_closed()
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
        self.shared.borrow_mut().sender_count -= 1;
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T: Clone> OneshotReceiver<T> for Receiver<T> {
    async fn recv(&self) -> Option<T> {
        let shared = self.shared.borrow();

        if let Some(value) = shared.value.as_ref() {
            return Some(value.clone());
        }

        let listener = shared.notify.listen();
        drop(shared);

        if let Some(listener) = listener {
            listener.await;
        }

        self.shared.borrow().value.clone()
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        let shared = self.shared.borrow();

        if let Some(value) = shared.value.as_ref() {
            return Ok(value.clone());
        }
        if shared.is_closed() {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }

    fn is_closed(&self) -> bool {
        self.shared.borrow().is_closed()
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


mod shared {
    use event_listener::{Event, EventListener};

    #[derive(Debug)]
    pub struct Shared<T> {
        pub value: Option<T>,
        pub notify: Notify,
        pub sender_count: usize,
        pub receiver_count: usize,
    }

    impl<T> Shared<T> {
        pub fn is_closed(&self) -> bool {
            self.sender_count == 0 || self.receiver_count == 0
        }
    }


    #[derive(Debug)]
    pub struct Notify {
        // TODO(perf): replace with a not thread-safe intrusive list?
        event: Event,
        notified: bool,
    }

    impl Notify {
        pub fn new() -> Self {
            Self {
                event: Event::new(),
                notified: false,
            }
        }

        pub fn listen(&self) -> Option<EventListener> {
            if self.notified {
                return None;
            }

            Some(self.event.listen())
        }

        pub fn notify(&mut self) {
            self.notified = true;
            self.event.notify(usize::MAX);
        }
    }
}
