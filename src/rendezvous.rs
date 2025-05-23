use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::default());
    (inner.clone().into(), inner.into())
}


pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Sender<T> {
    pub async fn send(&mut self, value: T) -> Result<(), T> {
        self.inner.sender_notify.notified().await;

        if !self.inner.open.load(Acquire) {
            self.inner.sender_notify.notify_last();
            return Err(value);
        }

        **self.inner.value.lock().unwrap() = Some(value);
        self.inner.receiver_notify.notify_last();

        Ok(())
    }
}

impl<T> From<Arc<Inner<T>>> for Sender<T> {
    fn from(inner: Arc<Inner<T>>) -> Self {
        Self { inner }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.inner.open.store(false, Release);
        self.inner.receiver_notify.notify_last();
    }
}


pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Receiver<T> {
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.sender_notify.notify_last();
        self.inner.receiver_notify.notified().await;

        if let Some(value) = self.inner.value.lock().unwrap().take() {
            return Some(value);
        }

        debug_assert!(!self.inner.open.load(Acquire));
        self.inner.receiver_notify.notify_last();
        None
    }
}

impl<T> From<Arc<Inner<T>>> for Receiver<T> {
    fn from(inner: Arc<Inner<T>>) -> Self {
        Self { inner }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.open.store(false, Release);
        self.inner.sender_notify.notify_last();
    }
}


struct Inner<T> {
    value: Mutex<Box<Option<T>>>,
    sender_notify: Notify,
    receiver_notify: Notify,
    open: AtomicBool,
}

impl<T> Default for Inner<T> {
    fn default() -> Self {
        Self {
            value: Mutex::default(),
            sender_notify: Notify::default(),
            receiver_notify: Notify::default(),
            open: AtomicBool::new(true),
        }
    }
}
