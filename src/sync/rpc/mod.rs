use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::mpsc;
use std::fmt::{Display, Formatter};

/// Failure on RPC call.
#[derive(Debug, Copy, Clone)]
pub(crate) enum SendError {
    /// Channel is closed, unable to make a request.
    Closed,

    /// There was no response for this request.
    ///
    /// Channel might be closed.
    NoResponse,
}

impl Display for SendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Closed => write!(f, "RPC channel is closed"),
            SendError::NoResponse => write!(f, "no response for a given request"),
        }
    }
}

impl std::error::Error for SendError {}


/// A client to make sync and async calls.
pub(crate) trait RemoteClient<T, R, C: RemoteCallback<R>>: Clone
where
    T: SendOnMt + Unpin + 'static,
    R: SendOnMt + Unpin + 'static,
{
    /// Create a new client with a specified `sender`.
    fn new(sender: mpsc::unbounded::Sender<RemoteCall<T, C>>) -> Self;

    /// Make a call with provided `args` and wait for the result.
    /// 
    /// # Cancel safety.
    /// 
    /// This method is cancel safe, but, obviously, the result is going to be lost.
    fn send(&self, args: T) -> impl Future<Output = Result<R, SendError>> + SendOnMt;

    /// Make a call with provided `args`, but ignore the response.
    fn send_and_forget(&self, args: T) -> Result<(), SendError>;
}

/// Callback to a specific RPC call.
pub(crate) trait RemoteCallback<R> {
    fn on_result(self, result: R);
}

pub(crate) struct RemoteCall<T, C> {
    pub args: T,
    pub callback: Option<C>,
}


conditional! {
    multithread,

    pub(crate) mod mt;

    /// [RemoteCall], its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type Remote<T, R> = mt::Remote<T, R>;

    pub(crate) type Callback<R> = mt::Callback<R>;

    pub(crate) type Call<T, R> = mt::Call<T, R>;
}

conditional! {
    not(multithread),

    pub(crate) mod st;

    /// [RemoteCall], its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type Remote<T, R> = st::Remote<T, R>;

    pub(crate) type Callback<R> = st::Callback<R>;

    pub(crate) type Call<T, R> = st::Call<T, R>;
}
