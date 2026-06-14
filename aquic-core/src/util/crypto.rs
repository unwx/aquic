use core::fmt;
use hybrid_array::{Array, ArraySize};
use rand::TryRng;
use rand::rngs::SysRng;
use std::fmt::Debug;
use std::fmt::Formatter;

/// Key-store, provides active and replaced keys,
/// allows active key rotation.
pub trait KeyStore {
    type KeySize: ArraySize;

    /// Returns an active key
    /// that should be used for further operations.
    fn active(&self) -> &Array<u8, Self::KeySize>;

    /// Returns passive keys.
    ///
    /// These keys are going to be used only
    /// when the active key fails (for example at decrypting, hash results, etc).
    ///
    /// These keys will never be used for encryption and active cryptography.
    fn passive(&self) -> impl Iterator<Item = &Array<u8, Self::KeySize>> + '_;

    /// Returns the current revision of the keystore.
    /// If this number changes, the active and passive keys should be fetched again.
    fn revision(&self) -> u64;
}


/// A simple [KeyStore] implementation.
///
/// Randomly generates a single key,
/// that lives and is used as long as `Self` exists.
pub struct RandomKey<S: ArraySize>(Array<u8, S>);

impl<S: ArraySize> KeyStore for RandomKey<S> {
    type KeySize = S;

    fn active(&self) -> &Array<u8, Self::KeySize> {
        self.0.as_ref()
    }

    fn passive(&self) -> impl Iterator<Item = &Array<u8, Self::KeySize>> + '_ {
        std::iter::once(self.0.as_ref())
    }

    fn revision(&self) -> u64 {
        0
    }
}

impl<S: ArraySize> Default for RandomKey<S> {
    fn default() -> Self {
        let mut key = Array::default();
        SysRng
            .try_fill_bytes(key.as_mut())
            .expect("SysRng failed to fill bytes");
        Self(key)
    }
}

impl<S: ArraySize> Debug for RandomKey<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RandomKey").finish()
    }
}

impl<S: ArraySize> Drop for RandomKey<S> {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
