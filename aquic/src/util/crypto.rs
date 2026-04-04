use core::fmt;
use rand::TryRng;
use rand::rngs::SysRng;
use std::fmt::Debug;
use std::fmt::Formatter;

// TODO(refactor): use hybrid-array::Array,
// when `aead` and `digest` crates move from generic-array to the hybrid-array.

/// Crypto keys consumer.
///
/// The keys size must be constant and never change.
/// If the keys size don't much the choosen crypto algorithm, it will lead to panics.
pub trait KeyConsumer {
    /// Returns an active key
    /// that should be used for further operations.
    fn active(&self) -> &[u8];

    /// Returns passive keys.
    ///
    /// These keys are going to be used only
    /// when the active key fails (for example at decrypting, hash results, etc).
    fn passive(&self) -> impl Iterator<Item = &[u8]> + '_;

    /// Returns the current revision of the keystore.
    /// If this number changes, the active and passive keys should be fetched again.
    fn revision(&self) -> u64;
}


/// A simple [KeyConsumer] implementation.
///
/// Randomly generates a single key,
/// that lives and is used as long as `Self` exists.
pub struct RandomKey<const SIZE: usize>([u8; SIZE]);

impl<const SIZE: usize> KeyConsumer for RandomKey<SIZE> {
    fn active(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn passive(&self) -> impl Iterator<Item = &[u8]> + '_ {
        std::iter::once(self.0.as_ref())
    }

    fn revision(&self) -> u64 {
        0
    }
}

impl<const SIZE: usize> Default for RandomKey<SIZE> {
    fn default() -> Self {
        let mut key = [0u8; SIZE];
        SysRng.try_fill_bytes(&mut key).unwrap();
        Self(key)
    }
}

impl<const SIZE: usize> Debug for RandomKey<SIZE> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RandomKey").finish()
    }
}
