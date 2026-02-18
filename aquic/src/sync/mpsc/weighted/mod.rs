use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::{SendError, TryRecvError, TrySendError};

/// A sending part of weighted `mpmc` channel.
pub(crate) trait WeightedSender<T>: Clone {
    /// Sends a value with a specified custom weight of this item,
    /// where `weight` can be any value, including zero.
    ///
    /// Will block if
    /// - `current_weight != 0 && current_weight + value_weight > bound`, or
    /// - `current_weight + value_weight` results in overflow.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that no messages were received on this channel
    /// if the future drops.
    fn send(
        &self,
        value: T,
        weight: usize,
    ) -> impl Future<Output = Result<(), SendError<T>>> + SendOnMt;

    /// Tries to send a value with a specified custom weight of this item,
    /// where `weight` can be any value, including zero.
    ///
    /// Will return [TrySendError::Full] if
    /// - `current_weight != 0 && current_weight + value_weight > bound`, or
    /// - `current_weight + value_weight` results in overflow.
    fn try_send(&self, value: T, weight: usize) -> Result<(), TrySendError>;

    /// Returns total weight of all pending items in the channel.
    fn occupation(&self) -> usize;

    /// Returns `true` if no [`WeightedReceiver`] exists.
    fn is_closed(&self) -> bool;
}

/// A receiving part of weighted `mpmc` channel.
pub(crate) trait WeightedReceiver<T: SendOnMt + Unpin + 'static> {
    /// Waits and receive a message, or returns `None` if channel is closed.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that no messages were received on this channel
    /// if the future drops.
    fn recv(&mut self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Tries to receive a message immediately.
    fn try_recv(&mut self) -> Result<T, TryRecvError>;

    /// Returns total weight of all pending items in the channel.
    fn occupation(&self) -> usize;

    /// Returns `true` if no [`WeightedSender`] exists.
    fn is_closed(&self) -> bool;
}


conditional! {
    multithread,

    mod mt;

    /// Async runtime agnostic `weighted::Sender`.
    pub(crate) type Sender<T> = mt::Sender<T>;

    /// Async runtime agnostic `weighted::Receiver`.
    pub(crate) type Receiver<T> = mt::Receiver<T>;

    /// Creates a special bounded `mpsc` channel.
    ///
    /// Comparing to other bounded channels,
    /// this one allows more control over channel's capacity by specifying the weight (size, or cost of being in the channel) for each item inside.
    ///
    /// The `bound` specifies the limit before senders block.
    #[inline]
    pub(crate) fn channel<T: Send + Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
        mt::channel(bound)
    }
}

conditional! {
    not(multithread),

    mod st;

    /// Async runtime agnostic `weighted::Sender`.
    pub(crate) type Sender<T> = st::Sender<T>;

    /// Async runtime agnostic `weighted::Receiver`.
    pub(crate) type Receiver<T> = st::Receiver<T>;

    /// Creates a special bounded `mpsc` channel.
    ///
    /// Comparing to other bounded channels,
    /// this one allows more control over channel's capacity by specifying the weight (size, or cost of being in the channel) for each item inside.
    ///
    /// The `bound` specifies the limit before senders block.
    #[inline]
    pub(crate) fn channel<T: Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
        st::channel(bound)
    }
}


/// Returns true if channel has capacity to send one more message with weight `item_weight`.
#[inline]
#[rustfmt::skip]
fn has_capacity(current_occupation: usize, item_weight: usize, bound: usize) -> bool {
    if current_occupation == 0 {
        return true;
    }

    let total = current_occupation.checked_add(item_weight);
    if let Some(total) = total && total <= bound {
        return true;
    }

    false
}
