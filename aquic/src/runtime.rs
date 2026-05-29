use crate::conditional;
use std::time::Instant;

conditional! {
    any(feature = "async-send"),

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
    not(any(feature = "async-send")),

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


/// Async Runtime API.
pub trait AsyncRuntime {
    /// Spawns a new asynchronous task, that doesn't return anything.
    fn spawn_void<F>(future: F)
    where
        F: Future + SendOnMt + 'static,
        F::Output: SendOnMt + 'static;

    /// Waits until `deadline` is reached.
    fn sleep_until(deadline: Instant) -> impl Future + SendOnMt;
}
