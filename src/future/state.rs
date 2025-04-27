use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub struct Switch<T: Clone> {
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
        let get_state = || {
            let guard = self.state.lock().unwrap();
            guard.as_ref().cloned()
        };

        if let Some(state) = get_state() {
            return state;
        }

        self.notify.notified().await;
        get_state().expect("after receiving a notification there must be Some state")
    }

    pub fn is_switched(&self) -> bool {
        let guard = self.state.lock().unwrap();
        guard.is_some()
    }
}

impl<T: Clone> Default for Switch<T> {
    fn default() -> Self {
        Self::new()
    }
}
