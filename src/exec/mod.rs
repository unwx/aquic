use std::time::{Duration, Instant};
use crate::conditional;

conditional! {
    multithread,

    /// Conditionally [Send].
    ///
    /// Current async environment is multithreaded/work-stealing: [Send] is **required**.
    pub trait SendOnMt: Send {}

    impl<T: Send> SendOnMt for T {}


    /// Conditionally [Sync].
    ///
    /// Current async environment is multithreaded/work-stealing: [Sync] is **required**.
    pub trait SyncOnMt: Sync {}

    impl<T: Sync> SyncOnMt for T {}
}

conditional! {
    not(multithread),

    /// Conditionally [Send].
    ///
    /// Current async environment is single-threaded/thread-per-core: [Send] is **not required**.
    pub trait SendOnMt {}

    impl<T> SendOnMt for T {}


    /// Conditionally [Sync].
    ///
    /// Current async environment is single-threaded/thread-per-core: [Sync] is **not required**.
    pub trait SyncOnMt {}

    impl<T> SyncOnMt for T {}
}


/// Provides an API to work with async runtime.
pub(crate) struct Runtime {}

#[cfg(feature = "tokio")]
impl Runtime {
    pub fn spawn_void<F>(future: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(future);
    }
    
    pub async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    pub async fn sleep_until(deadline: Instant) {
        tokio::time::sleep_until(deadline.into()).await;
    }
}

#[cfg(feature = "monoio")]
impl Runtime {
    pub fn spawn_void<F>(future: F)
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        monoio::spawn(future);
    }

    pub async fn sleep(duration: Duration) {
        monoio::time::sleep(duration).await;
    }

    pub async fn sleep_until(deadline: Instant) {
        monoio::time::sleep_until(deadline.into()).await;
    }
}
