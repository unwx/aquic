use crate::sync::oneshot::mt::shared::{Notify, Shared};
use crate::sync::oneshot::{OneshotReceiver, OneshotSender, SendError, TryRecvError};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::{Arc, OnceLock};

#[rustfmt::skip]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        value: OnceLock::new(),
        notify: Notify::new(),
        sender_count: AtomicUsize::new(1),
        receiver_count: AtomicUsize::new(1),
    });

    let sender = Sender { shared: shared.clone() };
    let receiver = Receiver { shared };

    (sender, receiver)
}


#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Send + Sync> OneshotSender<T> for Sender<T> {
    fn send(&self, value: T) -> Result<(), SendError> {
        if self.is_closed() {
            return Err(SendError::Closed);
        }

        if self.shared.value.set(value).is_err() {
            return Err(SendError::Full);
        }

        self.shared.notify.notify();
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.shared.receiver_count.load(Acquire) == 0
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.sender_count.fetch_add(1, Relaxed);

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.sender_count.fetch_sub(1, Release);
    }
}


#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone + Send + Sync> OneshotReceiver<T> for Receiver<T> {
    async fn recv(&self) -> Option<T> {
        if let Some(value) = self.shared.value.get() {
            return Some(value.clone());
        }

        self.shared.notify.wait().await;
        self.shared.value.get().cloned()
    }

    fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(value) = self.shared.value.get() {
            return Ok(value.clone());
        }

        if self.is_closed() {
            if let Some(value) = self.shared.value.get() {
                return Ok(value.clone());
            }

            return Err(TryRecvError::Closed);
        }

        Err(TryRecvError::Empty)
    }

    fn is_closed(&self) -> bool {
        self.shared.sender_count.load(Acquire) == 0
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.receiver_count.fetch_add(1, Relaxed);

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.receiver_count.fetch_sub(1, Release);
    }
}


mod shared {
    use event_listener::Event;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering::{Acquire, Release};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[derive(Debug)]
    pub struct Shared<T> {
        pub value: OnceLock<T>,
        pub notify: Notify,
        pub sender_count: AtomicUsize,
        pub receiver_count: AtomicUsize,
    }


    #[derive(Debug)]
    pub struct Notify {
        event: Event,
        notified: AtomicBool,
    }

    impl Notify {
        pub fn new() -> Self {
            Self {
                event: Event::new(),
                notified: AtomicBool::new(false),
            }
        }

        pub async fn wait(&self) {
            if self.notified.load(Acquire) {
                return;
            }

            let listener = self.event.listen();

            if self.notified.load(Acquire) {
                return;
            }

            listener.await;
        }

        pub fn notify(&self) {
            self.notified.store(true, Release);
            self.event.notify(usize::MAX);
        }
    }
}
