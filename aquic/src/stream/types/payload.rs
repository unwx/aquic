use crate::Estimate;

/// Represents a piece of data received from a stream.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Payload<T> {
    /// An intermediate chunk of data.
    ///
    /// The stream direction remains open and more data is expected to follow.
    /// This corresponds to a frame with the `FIN` bit set to **0**.
    Item(T),

    /// The final chunk of data.
    ///
    /// This contains payload data, but also signals the end of the stream direction.
    /// No more data will arrive after this.
    /// This corresponds to a frame with data and the `FIN` bit set to **1**.
    Last(T),

    /// A termination signal with no data.
    ///
    /// The stream direction has closed successfully without delivering a final payload.
    /// This corresponds to an empty frame with the `FIN` bit set to **1**.
    Done,
}

impl<T> Payload<T> {
    /// Returns `true` if this item represents the end of the stream direction ([`Payload::Last`] or [`Payload::Done`]).
    #[inline]
    pub fn is_fin(&self) -> bool {
        matches!(self, Self::Last(_) | Self::Done)
    }

    /// Returns the inner data if present, discarding the `FIN` state.
    #[inline]
    pub fn into_inner(self) -> Option<T> {
        match self {
            Self::Item(val) | Self::Last(val) => Some(val),
            Self::Done => None,
        }
    }

    /// Maps the inner value to a new type, preserving the stream direction state.
    #[inline]
    pub fn map<U, F>(self, f: F) -> Payload<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Self::Item(t) => Payload::Item(f(t)),
            Self::Last(t) => Payload::Last(f(t)),
            Self::Done => Payload::Done,
        }
    }

    /// Maps the inner value to a new type, allowing the mapping function to return a Result.
    #[inline]
    pub fn try_map<U, E, F>(self, f: F) -> Result<Payload<U>, E>
    where
        F: FnOnce(T) -> Result<U, E>,
    {
        Ok(match self {
            Self::Item(t) => Payload::Item(f(t)?),
            Self::Last(t) => Payload::Last(f(t)?),
            Self::Done => Payload::Done,
        })
    }
}

impl<T: Estimate> Estimate for Payload<T> {
    fn estimate(&self) -> usize {
        match self {
            Payload::Item(it) => it.estimate(),
            Payload::Last(it) => it.estimate(),
            Payload::Done => 0,
        }
    }
}
