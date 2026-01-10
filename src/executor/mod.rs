/// Conditionally [Send].
///
/// Current async runtime environment **requires** [Send] to spawn a future,
/// therefore [Send] must be present.
#[cfg(send_rt)]
pub trait SendIfRt: Send {}

#[cfg(send_rt)]
impl<T: Send> SendIfRt for T {}

/// Conditionally [Send].
///
/// Current async runtime environment **does not require** [Send] to spawn a future,
/// therefore [Send] can be absent.
#[cfg(not(send_rt))]
pub trait SendIfRt {}

#[cfg(not(send_rt))]
impl<T> SendIfRt for T {}


/// Executor that provides an API to spawn async tasks,
/// and some utilities to work with them.
pub(crate) struct Executor {}

impl Executor {
    /// [tokio::spawn].
    #[cfg(feature = "tokio-rt")]
    pub fn spawn_void<F>(future: F)
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(future);
    }

    /// [monoio::spawn].
    #[cfg(feature = "monoio-rt")]
    pub fn spawn_void<F>(future: F)
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        monoio::spawn(future);
    }
}
