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


/// Executor that provides an API to spawn async tasks,
/// and some utilities to work with them.
pub(crate) struct Executor {}

impl Executor {
    /// [tokio::spawn].
    #[cfg(feature = "tokio")]
    pub fn spawn_void<F>(future: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(future);
    }

    /// [monoio::spawn].
    #[cfg(feature = "monoio")]
    pub fn spawn_void<F>(future: F)
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        monoio::spawn(future);
    }
}
