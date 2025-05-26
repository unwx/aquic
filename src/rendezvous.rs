use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

#[rustfmt::skip]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new());
    let sender = Sender { inner: inner.clone(), };
    let receiver = Receiver { inner };

    (sender, receiver)
}


pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Sender<T> {
    // TODO(docs): not cancel-safe.
    //  Please 'drop()' or 'close()' after a cancellation.
    pub async fn send(&mut self, value: T) -> bool {
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.open {
                return false;
            }

            if state.value.is_some() {
                Self::close_with_mutex(state, &self.inner.send_done);
                panic!("rendezvous::send is not cancel-safe: detected an attempt to rewrite sent value");
            }

            state.value = Some(value);
        }

        self.inner.send_done.notify_last();
        self.inner.recv_done.notified().await;

        self.inner.state.lock().unwrap().delivered
    }

    pub fn close(&mut self) {
        Self::close_with_mutex(
            self.inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            &self.inner.send_done,
        );
    }

    fn close_with_mutex(mut state: MutexGuard<State<T>>, notify: &Notify) {
        state.open = false;
        drop(state);

        notify.notify_last();
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.close();
    }
}


pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Receiver<T> {
    // TODO(docs): cancel-safe.
    pub async fn recv(&mut self) -> Option<T> {
        self.inner.send_done.notified().await;

        let mut state = self.inner.state.lock().unwrap();
        state.delivered = false;

        if let Some(value) = state.value.take() {
            state.delivered = true;
            drop(state);

            self.inner.recv_done.notify_last();
            return Some(value);
        }

        let open = state.open;
        drop(state);

        if open {
            panic!("there must be a value after 'send_done' notification");
        } else {
            self.inner.send_done.notify_last();
            None
        }
    }

    pub fn close(&mut self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());

            state.open = false;
            state.delivered = state.value.is_none();
            state.value = None;
        }

        self.inner.recv_done.notify_last();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}


struct Inner<T> {
    state: Mutex<State<T>>,
    send_done: Notify,
    recv_done: Notify,
}

impl<T> Inner<T> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                value: None,
                open: true,
                delivered: false,
            }),
            send_done: Notify::new(),
            recv_done: Notify::new(),
        }
    }
}


struct State<T> {
    value: Option<T>,
    open: bool,
    delivered: bool,
}
