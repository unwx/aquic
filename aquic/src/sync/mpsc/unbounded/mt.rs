use crossfire::mpsc::List;

use crate::sync::mpsc::unbounded::{UnboundedReceiver, UnboundedSender};
use crate::sync::mpsc::{SendError, TryRecvError};

pub(crate) fn channel<T: Send + Unpin + 'static>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = crossfire::mpsc::unbounded_async();
    (Sender { inner: sender }, Receiver { inner: receiver })
}


pub(crate) struct Sender<T: Send + Unpin + 'static> {
    inner: crossfire::MTx<List<T>>,
}

impl<T: Send + Unpin + 'static> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send + Unpin + 'static> UnboundedSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError> {
        self.inner.send(value).map_err(|_| SendError)
    }

    fn is_closed(&self) -> bool {
        self.inner.is_disconnected()
    }
}


pub(crate) struct Receiver<T: Send + Unpin + 'static> {
    inner: crossfire::AsyncRx<List<T>>,
}

impl<T: Send + Unpin + 'static> UnboundedReceiver<T> for Receiver<T> {
    async fn recv(&mut self) -> Option<T> {
        // crossfire.recv() is cancellation safe.
        self.inner.recv().await.ok()
    }

    fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.inner.try_recv().map_err(|e| match e {
            crossfire::TryRecvError::Empty => TryRecvError::Empty,
            crossfire::TryRecvError::Disconnected => TryRecvError::Closed,
        })
    }

    fn is_closed(&self) -> bool {
        self.inner.is_disconnected()
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
