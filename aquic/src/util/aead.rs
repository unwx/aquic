use crate::util::KeyConsumer;
use aead::AeadInPlace;
use aead::KeyInit;
use aead::generic_array::GenericArray;
use rand::Rng;
use rand::rng;
use std::fmt;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::ops::Add;
use typenum::Unsigned;

/// AEAD cipher, provides encryption & decryption API
/// and allows manual key rotation using [KeyConsumer].
pub struct Aead<A, K> {
    ciphers: Vec<Cipher<A>>,
    keys: K,
    revision: u64,
}

impl<A, K> Aead<A, K>
where
    A: KeyInit + AeadInPlace,
    K: KeyConsumer,
{
    /// Creates a new cipher instance.
    pub fn new(keys: K) -> Self {
        let mut this = Self {
            ciphers: Vec::new(),
            keys,
            revision: 0,
        };

        this.update_ciphers();
        this
    }

    /// Applies AEAD encryption on `&mut plaintext`.
    ///
    /// Authentication tag is computed based on `associated_data` + `plaintext` + `nonce`;
    /// nonce is generated automatically, using [`ThreadRng`][rand::rngs::ThreadRng].
    ///
    /// Secure tag and nonce will be automatically appended at the end of the encrypted buffer,
    /// `&mut plaintext`.
    pub fn encrypt_and_pack(&mut self, associated_data: &[u8], plaintext: &mut Vec<u8>) {
        self.compare_and_update_ciphers();
        self.ciphers[0].encrypt_and_pack(associated_data, plaintext)
    }

    /// Extracts previously appended nonce and authentication tag,
    /// and decrypts the `&mut ciphertext.
    ///
    /// On success, `ciphertext` will only contain plain clean data.
    ///
    /// # Arguments
    ///
    /// * `associated_data`: associated data that is expected to match data during encryption.
    /// * `ciphertext`: encrypted payload, that presumably contains an auth tag and nonce.
    /// * `fallback_buffer`: a temporary buffer that recovers `ciphertext` state if decryption fails on the first attempt,
    ///   allowing to repeat attempt using an older key.
    ///
    /// Note: `ciphertext` and `fallback_buffer` may be left in "dirty" state on error,
    /// that is, partially decrypted, etc.
    pub fn unpack_and_decrypt(
        &mut self,
        associated_data: &[u8],
        ciphertext: &mut Vec<u8>,
        fallback_buffer: &mut Vec<u8>,
    ) -> Result<(), aead::Error> {
        self.compare_and_update_ciphers();

        fallback_buffer.clear();
        fallback_buffer.extend_from_slice(ciphertext);

        for cipher in &mut self.ciphers {
            if cipher
                .unpack_and_decrypt(associated_data, ciphertext)
                .is_ok()
            {
                fallback_buffer.clear();
                return Ok(());
            }

            ciphertext.clear();
            ciphertext.extend_from_slice(fallback_buffer);
        }

        fallback_buffer.clear();
        Err(aead::Error)
    }


    fn compare_and_update_ciphers(&mut self) {
        if self.revision != self.keys.revision() {
            self.update_ciphers();
        }
    }

    #[rustfmt::skip]
    fn update_ciphers(&mut self) {
        self.ciphers.clear();
        self.ciphers.push(Cipher::new(self.keys.active()));
        self.ciphers.extend(self.keys.passive().map(Cipher::new));
        self.revision = self.keys.revision();
    }
}

impl<A, K> Debug for Aead<A, K> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Aead").finish()
    }
}


#[derive(Clone)]
struct Cipher<A> {
    inner: A,
}

impl<A> Cipher<A>
where
    A: KeyInit + AeadInPlace,
{
    pub fn new(key: &[u8]) -> Self {
        let key = GenericArray::from_slice(key);
        Self { inner: A::new(key) }
    }

    pub fn encrypt_and_pack(&self, associated_data: &[u8], plaintext: &mut Vec<u8>) {
        let mut nonce = GenericArray::<u8, A::NonceSize>::default();
        rng().fill_bytes(&mut nonce);

        plaintext.reserve(
            Self::auth_tag_len()
                .add(Self::nonce_len())
                .add(Self::cipher_overhead()),
        );

        self.inner
            .encrypt_in_place(&nonce, associated_data, plaintext)
            .expect("AEAD 'encrypt_in_place' should never fail");

        plaintext.extend_from_slice(&nonce);
    }

    pub fn unpack_and_decrypt(
        &self,
        associated_data: &[u8],
        ciphertext: &mut Vec<u8>,
    ) -> Result<(), aead::Error> {
        if ciphertext.len() < Self::auth_tag_len() + Self::nonce_len() {
            return Err(aead::Error);
        }

        let nonce = GenericArray::<u8, A::NonceSize>::from_exact_iter(
            ciphertext.drain((ciphertext.len() - Self::nonce_len())..),
        )
        .expect("provided iterator must have the same size 'nonce' len");

        self.inner
            .decrypt_in_place(&nonce, associated_data, ciphertext)
    }


    pub const fn nonce_len() -> usize {
        <A::NonceSize as Unsigned>::USIZE
    }

    pub const fn auth_tag_len() -> usize {
        <A::TagSize as Unsigned>::USIZE
    }

    pub const fn cipher_overhead() -> usize {
        <A::CiphertextOverhead as Unsigned>::USIZE
    }
}
