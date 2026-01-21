use crate::backend::cid::{Bytes, ConnIdMeta};
use crate::backend::cid::{ConnIdError, ConnIdGenerator, ConnectionId, MAX_CID_LEN};
use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use bitvec::array::BitArray;
use bitvec::field::BitField;
use bitvec::order::Msb0;
use bitvec::prelude::BitVec;
use bitvec::view::BitView;
use rand::{RngCore, rng};
use std::time::Duration;

/// AES block size in bytes.
const AES_BLOCK_SIZE: usize = 16;

/// AES block size in bits (128).
const AES_BLOCK_SIZE_BITS: usize = AES_BLOCK_SIZE * 8;

/// Shortcut for AES block-sized bit array.
type Bits = BitArray<[u8; AES_BLOCK_SIZE], Msb0>;


/// [ConnIdMeta] for a server Connection ID.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ServerConnIdMeta {
    /// Server ID (may optionally include other data, as region).
    pub server_id: u32,

    /// A Core ID that is **mandatory** for routing in thread-per-core async runtimes like monoio,
    /// where multiple sockets (per core) binds on the same port.
    ///
    /// On single-thread/work-stealing runtimes it is not mandatory.
    pub core_id: Option<u16>,
}

impl ConnIdMeta for ServerConnIdMeta {
    fn core_id(&self) -> Option<u16> {
        self.core_id
    }
}


/// Load-balancer compatible implementation of the [ConnIdGenerator]
/// that should be sufficient for most cases: can store `server_id` and `core_id`.
///
/// In case you want to store advanced routing information like region, zone,
/// you might want to store it inside `server_id` too.
pub struct ServerConnIdGenerator {
    /// 16-byte key shared with the Load Balancer.
    key: [u8; 16],

    /// Your Server ID (Routing Tag).
    server_id: u32,

    /// Length of `server_id` in bits (8-32).
    server_id_bits: usize,

    /// Core ID for this specific thread/worker.
    core_id: u16,

    /// Length of `core_id` in bits (0-16).
    ///
    /// `0` means that `core_id` is not used.
    core_id_bits: usize,

    /// How many bits of purely random padding to add at the end.
    entropy_bits: usize,

    /// Generated Connection ID lifetime.
    lifetime: Option<Duration>,

    /// Pre-initialized cipher for speed.
    cipher: Aes128,
}

impl ServerConnIdGenerator {
    /// Creates new generator with a specified encryption key.
    ///
    /// * `server_id_len` The length of `server_id` in bits (8-32).
    /// * `core_id_bits` The length of `core_id` in bits (0-16), where 0 means `core_id` is not used.
    /// * `entropy_bits` The length of just random Nonce bits, minimum `32`.
    /// * `lifetime` Generated Connection ID lifetime.
    ///
    /// **Note**: `core_id` is an addition,
    /// and it's written in the place, [where `Nonce` should be written](https://datatracker.ietf.org/doc/html/draft-ietf-quic-load-balancers-14#name-cid-format).
    ///
    /// But it does not replace the `entropy_bits` value.
    /// That is,
    /// - if `core_id_bits` is `16`,
    /// - and `entropy_bits` is `34`,
    /// - then the resulting `Nonce` would be `50` bits long.
    pub fn new(
        key: [u8; 16],
        server_id: u32,
        core_id: u16,
        server_id_bits: usize,
        core_id_bits: usize,
        entropy_bits: usize,
        lifetime: Option<Duration>,
    ) -> Self {
        assert!(
            (8..=32).contains(&server_id_bits),
            "server_id_bits must be in range [8..=32]"
        );
        assert!(core_id_bits <= 16, "core_id_bits must be in range [0.=16]");
        assert!(entropy_bits >= 32, "entropy_bits must be in range [32..]");

        Self {
            key,
            server_id,
            core_id,
            server_id_bits,
            core_id_bits,
            entropy_bits,
            lifetime,
            cipher: Aes128::new(&GenericArray::from(key)),
        }
    }

    /// Helper to pack the bits: `[ FirstOctet | ServerID | CoreID | Padding ]`
    fn build_cid(&self) -> Bytes<MAX_CID_LEN> {
        let mut buf = Bytes::ZERO;
        buf[0] = rng().next_u32() as u8;

        let mut plaintext = buf[1..].view_bits_mut::<Msb0>();
        let mut offset = 0;

        plaintext[offset..offset + self.server_id_bits].store_be(self.server_id);
        offset += self.server_id_bits;

        if self.core_id_bits != 0 {
            plaintext[offset..offset + self.core_id_bits].store_be(self.core_id);
            offset += self.core_id_bits;
        }

        {
            let entropy_bits = (MAX_CID_LEN * 8)
                .saturating_sub(offset)
                .min(self.entropy_bits);

            // Currently, it should not be possible to have `entropy_bits` <= 32 here,
            // but it's good to ensure.
            assert!(
                entropy_bits >= 32,
                "at least 32 entropy_bits should be generated for a connection ID"
            );

            let entropy = Self::random_bits(entropy_bits);
            plaintext[offset..(offset + entropy_bits)].copy_from_bitslice(&entropy);
        }

        buf
    }

    fn random_bits(bits_len: usize) -> BitVec<u8, Msb0> {
        let bytes_len = bits_len.div_ceil(8);

        let mut bytes = vec![0u8; bytes_len];
        rng.fill_bytes(&mut bytes);

        let mut bits = BitVec::<u8, Msb0>::from_vec(bytes);
        bits.truncate(bits_len);

        bits
    }
}

impl ConnIdGenerator for ServerConnIdGenerator {
    type Meta = ServerConnIdMeta;

    fn generate_cid(&mut self) -> ConnectionId {
        let mut cid = self.build_cid();

        if cid.len() == 17 {
            let mut block = GenericArray::from_mut_slice(&mut [1..]);
            self.cipher.encrypt_block(&mut block);
        } else {
            encrypt_four_pass(&self.cipher, &mut cid);
        }

        ConnectionId(cid)
    }

    fn cid_len(&self) -> usize {
        let mut bits = 0;
        bits += self.server_id_bits;
        bits += self.core_id_bits;
        bits += self.entropy_bits;
        bits.div_ceil(8).min(MAX_CID_LEN)
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        self.lifetime
    }

    fn validate(&self, cid: &ConnectionId) -> bool {
        if self.cid_len() != cid.len() {
            return false;
        }

        match self.parse(cid) {
            Ok(meta) => {
                if meta.server_id != self.server_id {
                    return false;
                }
                if self.core_id_bits != 0 && meta.core_id != Some(self.core_id) {
                    return false;
                }

                true
            }
            Err(_) => false,
        }
    }

    fn decrypt(&self, cid: &mut ConnectionId) -> Result<(), ConnIdError> {
        let mut cid = &mut cid.0;

        if cid.len() == 17 {
            let mut block = GenericArray::from_mut_slice(&mut [1..]);
            self.cipher.decrypt_block(&mut block);
        } else {
            decrypt_four_pass(&self.cipher, &mut cid);
        }

        Ok(())
    }

    fn parse(&self, cid: &ConnectionId) -> Result<ServerConnIdMeta, ConnIdError> {
        if cid.len() == 0 {
            return Err(ConnIdError {
                original: cid.clone(),
                detail: "CID is empty".into(),
            });
        }

        let plaintext = cid.0[1..].view_bits::<Msb0>();
        let required_bits = self.server_id_bits + self.core_id_bits;

        if plaintext.len() < required_bits {
            return Err(ConnIdError {
                original: cid.clone(),
                detail: format!(
                    "Expected CID to have '{}' bits for server_id,\
                     '{}' bits for core_id, but the whole CID size in bits is '{}'",
                    self.server_id_bits,
                    self.core_id_bits,
                    plaintext.len()
                )
                .into(),
            });
        }

        let mut offset = 0;

        let server_id = plaintext[offset..offset + self.server_id_bits].load_be();
        offset += self.server_id_bits;

        let core_id = if self.core_id_bits > 0 {
            Some(plaintext[offset..offset + self.core_id_bits].load_be())
        } else {
            None
        };

        Ok(ServerConnIdMeta { server_id, core_id })
    }
}


// TODO(tests): high priority: do more tests.
// TODO(security): revisit this with a clean mind.

// Current implementation depends on `bitvec` trait and **theoretically** might be slower
// than implementation using u128.
//
// Also, `bitvec` uses `unsafe` under the hood, so, in theory, this should be replaced in the future,
// if it is possible to make **readable** (no bit-operations hell) version.

fn encrypt_four_pass(cipher: &Aes128, cid: &mut Bytes<20>) {
    let cid_len = cid.len as u8;

    let mut left = Bits::ZERO;
    let mut right = Bits::ZERO;
    let mut temp = Bits::ZERO;

    let plaintext = cid[1..].view_bits_mut::<Msb0>();
    let bits = plaintext.len();
    let left_bits = bits / 2;
    let right_bits = bits - left_bits;

    debug_assert!(left_bits <= AES_BLOCK_SIZE_BITS);
    debug_assert!(right_bits <= AES_BLOCK_SIZE_BITS);

    left[0..left_bits].copy_from_bitslice(&plaintext[..left_bits]);
    right[AES_BLOCK_SIZE_BITS - right_bits..].copy_from_bitslice(&plaintext[left_bits..]);

    let mut index = 0;
    while index < 4 {
        index += 1;
        temp = left;

        expand_left(&mut temp, cid_len, index);
        aes_ecb(&mut temp, cipher);
        truncate_right(&mut temp, right_bits);

        right ^= temp;
        index += 1;
        temp = right;

        expand_right(&mut temp, cid_len, index);
        aes_ecb(&mut temp, cipher);
        truncate_left(&mut temp, left_bits);

        left ^= temp;
    }

    plaintext[0..left_bits].copy_from_bitslice(&left[..left_bits]);
    plaintext[left_bits..].copy_from_bitslice(&right[AES_BLOCK_SIZE_BITS - right_bits..]);
}

fn decrypt_four_pass(cipher: &Aes128, cid: &mut Bytes<20>) {
    let cid_len = cid.len as u8;
    let plaintext = cid[1..].view_bits_mut::<Msb0>();

    let bits = plaintext.len();
    let left_bits = bits / 2;
    let right_bits = bits - left_bits;

    let mut left = Bits::ZERO;
    let mut right = Bits::ZERO;

    left[0..left_bits].copy_from_bitslice(&plaintext[..left_bits]);
    right[AES_BLOCK_SIZE_BITS - right_bits..].copy_from_bitslice(&plaintext[left_bits..]);

    let mut temp;
    let mut index = 4;

    while index > 0 {
        temp = right;

        expand_right(&mut temp, cid_len, index);
        aes_ecb(&mut temp, cipher);
        truncate_left(&mut temp, left_bits);
        left ^= temp;

        index -= 1;
        temp = left;

        expand_left(&mut temp, cid_len, index);
        aes_ecb(&mut temp, cipher);
        truncate_right(&mut temp, right_bits);
        right ^= temp;

        index -= 1;
    }

    plaintext[0..left_bits].copy_from_bitslice(&left[..left_bits]);
    plaintext[left_bits..].copy_from_bitslice(&right[AES_BLOCK_SIZE_BITS - right_bits..]);
}


fn aes_ecb(val: &mut Bits, cipher: &Aes128) {
    debug_assert_eq!(val.len(), AES_BLOCK_SIZE_BITS);

    let mut block = GenericArray::from_mut_slice(val.as_raw_mut_slice());
    cipher.encrypt_block(&mut block);
}

/// `truncate_left(0x2094842ca49256198c2deaa0ba53caa0, 28) = 0x20948420000000000000000000000000`.
fn truncate_left(val: &mut Bits, bits: usize) {
    val[bits..].fill(false);
}

/// `truncate_right(0x2094842ca49256198c2deaa0ba53caa0, 28) = 0x0000000000000000000000000a53caa0`.
fn truncate_right(val: &mut Bits, bits: usize) {
    let zero_count = AES_BLOCK_SIZE_BITS - bits;
    val[0..zero_count].fill(false);
}

/// `expand_left(0xaaba3c00000000000000000000000000, 0x0b, 0x02) = 0xaaba3c0000000000000000000000020b`.
/// * `msb_val`: The value to place in the most significant bits.
/// * `lsb_byte`: The byte to place in the lowest position.
/// * `second_lsb_byte`: The byte to place in the second-lowest position.
fn expand_left(msb_val: &mut Bits, lsb_byte: u8, second_lsb_byte: u8) {
    let mut slice = msb_val.as_raw_mut_slice();
    slice[AES_BLOCK_SIZE - 1] |= lsb_byte;
    slice[AES_BLOCK_SIZE - 2] |= second_lsb_byte;
}

/// `expand_right(0x00000000000000000000000000aaba3c, 0x0b, 0x02) = 0x0b020000000000000000000000aaba3c`.
/// * `lsb_val`: The value to place in the least significant bits.
/// * `msb_byte`: The byte to place in the highest position.
/// * `second_msb_byte`: The byte to place in the second-highest position.
fn expand_right(lsb_val: &mut Bits, msb_byte: u8, second_msb_byte: u8) {
    let mut slice = lsb_val.as_raw_mut_slice();
    slice[0] = msb_byte;
    slice[1] = second_msb_byte;
}
