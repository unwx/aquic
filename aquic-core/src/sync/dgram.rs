use crate::dgram::Error;
use crate::sync::mpmc::oneshot::{OneshotReceiver, OneshotSender};
use crate::sync::mpmc::watch::WatchSender;
use crate::sync::mpmc::{oneshot, watch};
use crate::sync::mpsc::weighted;
use crate::sync::mpsc::weighted::{WeightedReceiver, WeightedSender};
use crate::sync::{TryRecvError, TrySendError};
use crate::{Estimate, Spec};
use futures::{FutureExt, select_biased};
use std::any::type_name;
use std::borrow::Cow;
use std::thread::panicking;

/// Creates a `mpsc` channel for a single datagran direction communication.
///
/// This channel uses [Estimate][`crate::Estimate`] for each item.
///
/// The `bound` argument limits the weight of items that channel can hold.
/// It also may be `zero`, to hold no more than `1` item (unless its weight is `zero` too).
pub(crate) fn channel<S: Spec>(bound: usize) -> (Sender<S>, Receiver<S>) {
    let (item_sender, item_receiver) = weighted::channel(bound);
    let (error_sender, error_receiver) = oneshot::channel();
    let (max_dgram_size_sender, max_dgram_size_receiver) = watch::channel();

    let stream_sender = Sender {
        item_sender,
        error_sender: error_sender.clone(),
        error_receiver: error_receiver.clone(),
        max_dgram_size_receiver,
    };
    let stream_receiver = Receiver {
        item_receiver,
        error_sender,
        error_receiver,
        max_dgram_size_sender,
    };

    (stream_sender, stream_receiver)
}


/// A sending half of QUIC datagram channel.
///
/// Unlike [`stream::Sender`](crate::sync::stream::Sender),
/// datagram sender does not require manual termination before dropping it.
pub struct Sender<S: Spec> {
    /// Send items.
    item_sender: weighted::Sender<S::DgramItem>,

    /// Broadcast the closure event.
    error_sender: oneshot::Sender<Error>,

    /// Listen for closure events.
    error_receiver: oneshot::Receiver<Error>,

    /// Listen for maximum datagram size updates.
    max_dgram_size_receiver: watch::Receiver<usize>,
}

impl<S: Spec> Sender<S> {
    /// Sends an item that will be encoded into raw datagram.
    ///
    /// # Bounds
    ///
    /// The underlying channel is bounded and uses [Estimate] of the provided item,
    /// to see how much each item occupies the channel.
    ///
    /// Example:
    /// ```text
    /// item_1 weights 10.
    /// item_2 weights 15.
    /// item_3 weights 3.
    ///
    /// if sum(items) > bound: channel blocks until previous items are sent.
    /// ```
    ///
    /// **Notes**:
    /// - Item's weight may be `0`.
    /// - If `bound` is `0`, the channel will block if there is more than `1` item in the internal queue,
    /// **unless** its weight is `0`.
    ///
    /// # Returns
    ///
    /// - `Ok(())`: The item was enqueued for pick-up.
    /// - `Err(Error)`: The datagram direction is closed.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that the item will not be sent if the future drops.
    pub async fn send(&self, item: S::DgramItem) -> Result<(), Error> {
        let weight = item.estimate();

        select_biased! {
            err = self.error_receiver.recv().fuse() => {
                let err = err.unwrap_or_else(|| Self::err_rx_fallback_error());
                Err(self.close(err))
            },
            result = self.item_sender.send(item, weight).fuse() => {
                if result.is_err() {
                    return Err(self.close(Self::item_tx_fallback_error()));
                }

                Ok(())
            },
        }
    }

    /// Tries to send an item that will be encoded into raw datagram,
    /// if channel's capacity allows it.
    ///
    /// Returns `true` if the `item` was sent.
    ///
    /// More info: [`send()`](Self::send).
    pub fn try_send(&self, item: S::DgramItem) -> Result<bool, Error> {
        if let Some(e) = self.error() {
            return Err(e);
        }

        let weight = item.estimate();
        match self.item_sender.try_send(item, weight) {
            Ok(_) => Ok(true),
            Err(TrySendError::Full) => Ok(false),
            Err(TrySendError::Closed) => Err(self.close(Self::item_tx_fallback_error())),
        }
    }


    /// Terminates direction with [Error::Connection].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_connection(&self) {
        self.close(Error::Connection);
    }

    /// Terminates direction with [Error::HangUp].
    ///
    /// No-op if it's already closed.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns error if direction is closed.
    pub fn error(&self) -> Option<Error> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::err_rx_fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns an error receiver.
    ///
    /// This allows external observers to wait for the datagram direction to fail.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error> {
        self.error_receiver.clone()
    }


    /// Returns a maximum datagram size updates receiver.
    ///
    /// If encoded datagram size exceeds the maximum, it will be dropped.
    ///
    /// This allows external observers to react and adjust desired item's size.
    pub fn max_dgram_size_receiver(&self) -> watch::Receiver<usize> {
        self.max_dgram_size_receiver.clone()
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.
    fn close(&self, error: Error) -> Error {
        match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(oneshot::SendError::Closed) => Self::err_rx_fallback_error(),
            Err(oneshot::SendError::Full(actual_error)) => actual_error,
        }
    }


    // Fallback error is HangUp,
    // because Sender/Receiver would set 'End' first before being dropped.

    fn err_rx_fallback_error() -> Error {
        Error::HangUp(format!("'{}'.error_receiver is unavailable", type_name::<Self>()).into())
    }

    fn item_tx_fallback_error() -> Error {
        Error::HangUp(format!("'{}'.item_sender is unavailable", type_name::<Self>()).into())
    }
}

impl<S: Spec> Drop for Sender<S> {
    fn drop(&mut self) {
        let error: Error = if panicking() {
            Error::HangUp(format!("'{}' is dropped due to panic", type_name::<Self>()).into())
        } else {
            Error::End
        };

        self.close(error);
    }
}


/// A receiving half of QUIC datagram channel.
///
/// Unlike [`stream::Receiver`](crate::sync::stream::Receiver),
/// datagram receiver does not require manual termination before dropping it.
pub struct Receiver<S: Spec> {
    /// Receive actual data chunks.
    item_receiver: weighted::Receiver<S::DgramItem>,

    /// Broadcast the closure event.
    error_sender: oneshot::Sender<Error>,

    /// Listen for closure events.
    error_receiver: oneshot::Receiver<Error>,

    /// Send maximum datagram size updates.
    max_dgram_size_sender: watch::Sender<usize>,
}

impl<S: Spec> Receiver<S> {
    /// Receives a next datagram item.
    ///
    /// # Cancel Safety
    ///
    /// It is guaranteed that no messages were received on this channel if future drops.
    pub async fn recv(&mut self) -> Result<S::DgramItem, Error> {
        select_biased! {
            err = self.error_receiver.recv().fuse() => {
                let err = err.unwrap_or_else(|| Self::err_rx_fallback_error());
                Err(self.close(err))
            }
            result = self.item_receiver.recv().fuse() => {
                match result {
                    Some(item) => {
                        Ok(item)
                    }
                    None => {
                        let err = Self::item_rx_fallback_error();
                        Err(self.close(err))
                    }
                }
            }
        }
    }

    /// Tries to receive a next chunk of data.
    ///
    /// More: [`recv()`][Receiver::recv].
    pub fn try_recv(&mut self) -> Result<Option<S::DgramItem>, Error> {
        match self.error_receiver.try_recv() {
            Ok(e) => {
                return Err(self.close(e));
            }
            Err(TryRecvError::Closed) => {
                return Err(self.close(Self::err_rx_fallback_error()));
            }
            Err(TryRecvError::Empty) => {
                // Do nothing, channel is open.
            }
        };

        match self.item_receiver.try_recv() {
            Ok(item) => Ok(Some(item)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Closed) => Err(self.close(Self::item_rx_fallback_error())),
        }
    }


    /// Terminates direction with [Error::Connection].
    ///
    /// No-op if it's already closed.
    pub(crate) fn terminate_due_connection(&self) {
        self.close(Error::Connection);
    }

    /// Terminates direction with [Error::HangUp].
    ///
    /// No-op if it's already closed.
    pub(crate) fn hangup(&mut self, message: Cow<'static, str>) {
        self.close(Error::HangUp(message));
    }


    /// Returns error if direction is closed.
    pub fn error(&self) -> Option<Error> {
        match self.error_receiver.try_recv() {
            Ok(err) => Some(err),
            Err(TryRecvError::Closed) => Some(Self::err_rx_fallback_error()),
            Err(TryRecvError::Empty) => None,
        }
    }

    /// Returns an error receiver.
    ///
    /// This allows external observers to wait for the datagram direction to fail.
    pub fn error_receiver(&self) -> oneshot::Receiver<Error> {
        self.error_receiver.clone()
    }


    /// Sets the maximum datagram size and notifies the `Sender`.
    ///
    /// No-op if channel is closed.
    pub(crate) fn set_max_dgram_size(&self, value: usize) {
        let _ = self.max_dgram_size_sender.send(value);
    }


    /// Closes resources and returns an [Error]
    /// that is set for the whole channel.
    ///
    /// If the channel is closed, returns [Error] it was closed with.
    ///
    /// If the channel is still open, returns a clone of the provided `error`.
    fn close(&self, error: Error) -> Error {
        match self.error_sender.send(error.clone()) {
            Ok(_) => error,
            Err(oneshot::SendError::Closed) => Self::err_rx_fallback_error(),
            Err(oneshot::SendError::Full(actual_error)) => actual_error,
        }
    }


    // Fallback error is HangUp,
    // because Sender/Receiver would set 'End' first before being dropped.

    fn err_rx_fallback_error() -> Error {
        Error::HangUp(format!("'{}'.error_receiver is unavailable", type_name::<Self>()).into())
    }

    fn item_rx_fallback_error() -> Error {
        Error::HangUp(format!("'{}'.item_receiver is unavailable", type_name::<Self>()).into())
    }
}

impl<S: Spec> Drop for Receiver<S> {
    fn drop(&mut self) {
        let error: Error = if panicking() {
            Error::HangUp(format!("'{}' is dropped due to panic", type_name::<Self>()).into())
        } else {
            Error::End
        };

        self.close(error);
    }
}
