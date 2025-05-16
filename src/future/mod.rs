pub(crate) mod state;

use std::pin::Pin;
use std::task::{Context, Poll, Waker};

pub(crate) fn poll<T, F>(future: &mut F) -> Poll<T>
where
    F: Future<Output = T> + Unpin,
{
    let pin = Pin::new(future);
    let mut context = Context::from_waker(Waker::noop());
    pin.poll(&mut context)
}
