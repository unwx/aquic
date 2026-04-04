use std::{
    borrow::Borrow,
    fmt::{self, Display, Formatter},
};

/// An immutable stream ID.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(u64);

impl StreamId {
    /// The maximum possible stream ID value.
    pub const MAX: u64 = (1 << 62) - 1;

    /// Creates a new stream ID from a raw value,
    /// if the value doesn't exceed `2^62 - 1`.
    #[inline]
    pub fn new(value: u64) -> Result<Self, StreamIdError> {
        if value <= Self::MAX {
            return Ok(Self(value));
        }

        Err(StreamIdError)
    }

    /// Returns a new initial stream ID.
    #[inline]
    pub fn initial(client: bool, bidirectional: bool) -> Self {
        match (client, bidirectional) {
            (true, true) => Self::init_client_bidi(),
            (true, false) => Self::init_client_uni(),
            (false, true) => Self::init_server_bidi(),
            (false, false) => Self::init_server_uni(),
        }
    }

    /// Creates a new initial client bidirectional stream ID.
    #[inline]
    pub fn init_client_bidi() -> Self {
        Self(0)
    }

    /// Creates a new initial server bidirectional stream ID.
    #[inline]
    pub fn init_server_bidi() -> Self {
        Self(1)
    }

    /// Creates a new initial client unidirectional stream ID.
    #[inline]
    pub fn init_client_uni() -> Self {
        Self(2)
    }

    /// Creates a new initial server unidirectional stream ID.
    #[inline]
    pub fn init_server_uni() -> Self {
        Self(3)
    }


    /// Returns a raw `u64` stream ID value.
    #[inline]
    pub fn raw(&self) -> u64 {
        self.0
    }

    /// Returns `true` if the stream was initiated by the server connection.
    #[inline]
    pub fn is_server(&self) -> bool {
        (self.0 & 0x01) == 0x01
    }

    /// Returns `true` if the stream was initiated by the server connection.
    #[inline]
    pub fn is_client(&self) -> bool {
        !self.is_server()
    }

    /// Returns `true` if the stream is unidirectional.
    #[inline]
    pub fn is_uni(&self) -> bool {
        (self.0 & 0x02) == 0x02
    }

    /// Returns `true` if the stream is bidirectional.
    #[inline]
    pub fn is_bidi(&self) -> bool {
        !self.is_uni()
    }


    /// Returns the next stream ID in the sequence for the same initiator and direction.
    /// Returns `Err` if the next ID would exceed [`MAX value`](Self::MAX).
    #[inline]
    pub fn next(&self) -> Result<Self, StreamIdError> {
        let Some(value) = self.raw().checked_add(4) else {
            return Err(StreamIdError);
        };

        Self::new(value)
    }

    /// Returns the previous stream ID in the sequence for the same initiator and direction.
    /// Returns `Err` if the current ID is the first in its sequence.
    #[inline]
    pub fn previous(&self) -> Result<Self, StreamIdError> {
        let Some(value) = self.0.checked_sub(4) else {
            return Err(StreamIdError);
        };

        Self::new(value)
    }
}


impl Borrow<u64> for StreamId {
    fn borrow(&self) -> &u64 {
        &self.0
    }
}

impl Display for StreamId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}


macro_rules! impl_from {
    ($($target:ty),* $(,)?) => {
        $(
            impl From<$target> for StreamId {
                #[inline]
                fn from(value: $target) -> Self {
                    Self(value as u64)
                }
            }

            impl From<StreamId> for $target {
                #[inline]
                fn from(id: StreamId) -> Self {
                    id.0 as $target
                }
            }
        )*
    };
}

macro_rules! impl_try_from {
    ($($target:ty),* $(,)?) => {
        $(
            impl TryFrom<$target> for StreamId {
                type Error = StreamIdError;

                #[inline]
                fn try_from(value: $target) -> Result<Self, Self::Error> {
                    let value = u64::try_from(value).map_err(|_| StreamIdError)?;
                    Self::new(value as u64)
                }
            }

            impl From<StreamId> for $target {
                #[inline]
                fn from(id: StreamId) -> Self {
                    id.0 as $target
                }
            }
        )*
    };
}

impl_from!(u8, u16, u32);
impl_try_from!(u64, u128, usize);


#[derive(Debug, Copy, Clone)]
pub struct StreamIdError;

impl Display for StreamIdError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Stream ID must be in range of [0..=(2^62 - 1)]")
    }
}

impl std::error::Error for StreamIdError {}
