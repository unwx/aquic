use crate::conditional;
use crate::exec::SendOnMt;
use crate::sync::mpsc::{SendError, TryRecvError};

/// Sending part of the channel.
///
/// More info: [channel].
pub(crate) trait WeightedSender<T: SendOnMt + Unpin + 'static>: Clone {
    /// Send a value with a specified custom weight of this item,
    /// where `weight` can be any value, including zero.
    ///
    /// Returns `Ok` if value was sent, and `Err` if the channel is closed.
    ///
    /// Will block if
    /// - `current_weight != 0 && current_weight + value_weight > bound`, or
    /// - `current_weight + value_weight` results in overflow.
    ///
    /// # Cancel Safety
    ///
    /// Not cancel safe: may lead to lost message, or send a message after cancellation.
    fn send(
        &self,
        value: T,
        weight: usize,
    ) -> impl Future<Output = Result<(), SendError>> + SendOnMt;

    /// Returns total weight of all pending items in the channel.
    fn occupation(&self) -> usize;

    /// Returns `true` if channel is closed,
    ///
    /// and [Self::send] will always return `Err`.
    fn is_closed(&self) -> bool;
}

/// Receiving part of the channel.
///
/// More info: [channel].
pub(crate) trait WeightedReceiver<T: SendOnMt + Unpin + 'static> {
    /// Wait for an item and return:
    /// - `Ok` if it exists.
    /// - `Err` if channel is closed and there is no more items.
    ///
    /// # Cancel Safety
    ///
    /// Not cancel safe: may lead to lost message.
    fn recv(&mut self) -> impl Future<Output = Option<T>> + SendOnMt;

    /// Try to receive an item if it exists, and return:
    /// - `Ok` if it's present.
    /// - `Err(Closed)` if channel is closed.
    /// - `Err(Empty)` if there is no item available yet.
    fn try_recv(&mut self) -> Result<T, TryRecvError>;

    /// Returns total weight of all pending items in the channel.
    fn occupation(&self) -> usize;
}


conditional! {
    multithread,

    pub(crate) mod mt;

    /// Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type Sender<T> = mt::Sender<T>;

    /// Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **thread safe**.
    pub(crate) type Receiver<T> = mt::Receiver<T>;

    /// Creates a special bounded `mpsc` channel.
    ///
    /// Comparing to other bounded channels,
    /// this one allows more control over channel's capacity by specifying the weight (size, or cost of being in the channel) for each item inside.
    ///
    /// The `bound` specifies a limit before senders block.
    ///
    /// The channel won't allow the total weight of items to exceed `bound`,
    /// unless the current total weight is `0`.
    ///
    /// On current async environment it's **thread safe**,
    /// and intended to be used in multi-thread environments.
    pub(crate) fn channel<T: Send + Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
        mt::channel(bound)
    }
}

conditional! {
    not(multithread),

    pub(crate) mod st;

    /// Sender, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type Sender<T> = st::Sender<T>;

    /// Receiver, its concrete implementation depends on the current async environment.
    ///
    /// On current async env it's **not thread safe**.
    pub(crate) type Receiver<T> = st::Receiver<T>;

    /// Creates a special bounded `mpsc` channel.
    ///
    /// Comparing to other bounded channels,
    /// this one allows more control over channel's capacity by specifying the weight (size, or cost of being in the channel) for each item inside.
    ///
    /// The `bound` specifies a limit before senders block.
    ///
    /// The channel won't allow the total weight of items to exceed `bound`,
    /// unless the current total weight is `0`.
    ///
    /// On current async environment it's **not thread safe**,
    /// and intended to be used in single-thread environments.
    pub(crate) fn channel<T: Unpin + 'static>(bound: usize) -> (Sender<T>, Receiver<T>) {
        st::channel(bound)
    }
}


/// Returns true if the channel has capacity to send one more message with weight `item_weight`.
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
