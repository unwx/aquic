use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub struct Switch<T> {
    state: Arc<Mutex<Option<T>>>,
    notify: Arc<Notify>,
}

impl<T: Clone> Switch<T> {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn try_switch(&self, state: T) -> Result<(), T> {
        let mut guard = self.state.lock().unwrap();
        if let Some(state) = guard.as_ref().cloned() {
            return Err(state);
        }

        *guard = Some(state);
        drop(guard);

        self.notify.notify_waiters();
        Ok(())
    }

    pub async fn switched(&self) -> T {
        let get_state = || self.state.lock().unwrap().as_ref().cloned();

        if let Some(state) = get_state() {
            return state;
        }

        self.notify.notified().await;
        get_state().expect("after receiving a notification there must be Some state")
    }

    pub fn is_switched(&self) -> bool {
        self.state.lock().unwrap().is_some()
    }
}

impl<T: Clone> Default for Switch<T> {
    fn default() -> Self {
        Self::new()
    }
}


#[derive(Debug, Clone)]
pub struct Observer<T> {
    inner: T,
}

impl<T> Observer<Switch<T>> where T: Clone {
    pub async fn switched(&self) -> T {
        self.inner.switched().await
    }

    pub fn is_switched(&self) -> bool {
        self.inner.is_switched()
    }
}

impl<T: Clone> From<Switch<T>> for Observer<Switch<T>> {
    fn from(inner: Switch<T>) -> Self {
        Self { inner }
    }
}
