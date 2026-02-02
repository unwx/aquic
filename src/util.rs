use bytes::Bytes;

/// Represents a bytes container,
/// that may be owned, and may be not.
#[derive(Debug, Clone)]
pub enum CowBytes {
    /// Shared view to bytes.
    Shared(Bytes),

    /// Owned bytes.
    Owned(Vec<u8>),
}

impl CowBytes {
    /// Clones and returns owned bytes if `self` is [Shared](CowBytes::Shared),
    /// or simply transfers ownership if `self` if [Shared](CowBytes::Owned).
    pub fn into_owned(self) -> Vec<u8> {
        match self {
            Self::Shared(it) => Vec::from(it.as_ref()),
            Self::Owned(it) => it,
        }
    }
}
