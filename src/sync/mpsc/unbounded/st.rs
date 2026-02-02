use crate::sync::mpsc::unbounded::{UnboundedReceiver, UnboundedSender};
use crate::sync::mpsc::{SendError, TryRecvError};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Poll, Waker};

pub(crate) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        deque: VecDeque::new(),
        receiver_waker: None,
        receiver_exists: true,
    }));

    (
        Sender {
            shared: shared.clone(),
        },
        Receiver { shared },
    )
}


pub(crate) struct Sender<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T: Unpin + 'static> UnboundedSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError> {
        let mut shared = self.shared.borrow_mut();
        if !shared.receiver_exists {
            return Err(SendError);
        }

        shared.deque.push_back(value);
        if let Some(waker) = shared.receiver_waker.take() {
            waker.wake();
        }

        Ok(())
    }

    fn is_closed(&self) -> bool {
        !self.shared.borrow().receiver_exists
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
        let mut shared = self.shared.borrow_mut();

        #[allow(clippy::collapsible_if)]
        if Rc::strong_count(&self.shared) == 2 && shared.receiver_exists {
            if let Some(waker) = shared.receiver_waker.take() {
                waker.wake();
            }
        };
    }
}


pub(crate) struct Receiver<T> {
    shared: Rc<RefCell<Shared<T>>>,
}

impl<T> Receiver<T> {
    fn is_closed<S>(rc: &Rc<S>) -> bool {
        Rc::strong_count(rc) == 1
    }
}

impl<T: Unpin + 'static> UnboundedReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        poll_fn(|cx| {
            let mut shared = self.shared.borrow_mut();

            if let Some(value) = shared.deque.pop_front() {
                return Poll::Ready(Some(value));
            }
            if Self::is_closed(&self.shared) {
                return Poll::Ready(None);
            }

            shared.receiver_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let mut shared = self.shared.borrow_mut();

        if let Some(value) = shared.deque.pop_front() {
            return Ok(value);
        }
        if Self::is_closed(&self.shared) {
            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }

    fn is_closed(&self) -> bool {
        Self::is_closed(&self.shared)
    }

    fn len(&self) -> usize {
        self.shared.borrow().deque.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.deque.clear();
        shared.receiver_waker = None;
        shared.receiver_exists = false;
    }
}


struct Shared<T> {
    pub deque: VecDeque<T>,
    pub receiver_waker: Option<Waker>,
    pub receiver_exists: bool,
}
