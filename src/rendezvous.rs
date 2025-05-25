use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new());
    (inner.clone().into(), inner.into())
}


pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Sender<T> {
    pub async fn send(&mut self, value: T) -> Result<(), T> {
        self.inner.recv_ready.notified().await;
        let mut state = self.inner.state.lock().unwrap();

        // Receiver::recv was cancelled
        if state.value.is_some() {
            state.open = false;
            state.value = None;
        }

        if !state.open {
            drop(state);

            self.inner.recv_ready.notify_last();
            Err(value)
        } else {
            state.value = Some(value);
            drop(state);

            self.inner.send_done.notify_last();
            Ok(())
        }
    }
}

impl<T> From<Arc<Inner<T>>> for Sender<T> {
    fn from(inner: Arc<Inner<T>>) -> Self {
        Self { inner }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .open = false;

        self.inner.send_done.notify_last();
    }
}


pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Receiver<T> {
    // TODO(docs): not cancel-safe: risk of message loss.
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.recv_ready.notify_last();
        self.inner.send_done.notified().await;

        let mut state = self.inner.state.lock().unwrap();
        if let Some(value) = state.value.take() {
            return Some(value);
        }

        let open = state.open;
        drop(state);

        if open {
            // Panic outside the 'state' mutex, because there is no need to poison it.
            panic!("there must be a value after 'send_done' notification");
        } else {
            self.inner.send_done.notify_last();
            None
        }
    }
}

impl<T> From<Arc<Inner<T>>> for Receiver<T> {
    fn from(inner: Arc<Inner<T>>) -> Self {
        Self { inner }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());

            state.open = false;
            state.value = None;
        }

        self.inner.recv_ready.notify_last();
    }
}


struct Inner<T> {
    state: Mutex<State<T>>,
    recv_ready: Notify,
    send_done: Notify,
}

impl<T> Inner<T> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                value: None,
                open: true,
            }),
            recv_ready: Notify::new(),
            send_done: Notify::new(),
        }
    }
}


struct State<T> {
    value: Option<T>,
    open: bool,
}
