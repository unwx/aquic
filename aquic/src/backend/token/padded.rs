use crate::{
    backend::{
        Token, TokenError, TokenGenerator, TokenKind,
        cid::{ConnectionId, MAX_CID_LEN},
    },
    util::{Aead, Hmac, KeyConsumer},
};
use rand::{Rng, rng};
use std::{
    any::type_name,
    net::IpAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use typenum::Unsigned;

/// A padded QUIC token generator: all resulting tokens have the same length
/// and are highly obscure.
///
/// **It is highly recommended** to use one of the provided algorithms
/// for the underlying AEAD operation:
///
/// - [XChaCha20Poly1305](https://docs.rs/chacha20poly1305/latest/chacha20poly1305/type.XChaCha20Poly1305.html).
/// - [Aes256GcmSiv](https://docs.rs/aes-gcm-siv/latest/aes_gcm_siv/type.Aes256GcmSiv.html).
///
/// See [AeadCore::generate_nonce](aead::AeadCore::generate_nonce) security warning.
///
/// # Retry token
///
/// Structure:
/// ```text
/// Retry {
///   Peer Original DCID (0..160),
///   Padding (0..160),
///   Padding Length (8),
///   Expires At (64),
///   Auth Tag (?),
///   Nonce (?)
/// }
/// ```
///
/// `Auth Tag` also covers the next associated data fields:
/// - `Kind (Retry)`,
/// - `Peer Address`,
/// - `Peer Source Connection ID`,
/// - `Server New Source Connection ID`.
///
/// # Identity token
///
/// Structure:
/// ```text
/// Identity {
///   Padding (160),
///   Padding Length (8),
///   Expires At (64),
///   Auth Tag (?),
///   Nonce (?)
/// }
/// ```
///
/// `Auth Tag` also covers the next associated data fields:
/// - `Kind (Identity)`,
/// - `Peer Address`.
///
/// # Stateless Reset token
///
/// Uses HMAC function on server SCID.
pub struct PaddedTokenGenerator<A, M, AKeys, MKeys> {
    plaintext_buffer: Vec<u8>,
    associated_data_buffer: Vec<u8>,
    reserve_buffer: Vec<u8>,

    aead: Aead<A, AKeys>,
    hmac: Hmac<M, MKeys>,
}

impl<A, M, AKeys, MKeys> PaddedTokenGenerator<A, M, AKeys, MKeys>
where
    A: aead::KeyInit + aead::AeadInPlace,
    M: digest::KeyInit + digest::Mac,
    AKeys: KeyConsumer,
    MKeys: KeyConsumer,
{
    /// Creates a new instance of `PaddedTokenGenerator`.
    /// - AEAD is used for `Retry` packet and `NEW_TOKEN` frame tokens.
    /// - HMAC is used for reset tokens.
    ///
    /// # Panics
    ///
    /// If provided HMAC algorithm produces outputs shorter 16 bytes long.
    pub fn new(aead: Aead<A, AKeys>, hmac: Hmac<M, MKeys>) -> Self {
        if <M::OutputSize as Unsigned>::USIZE < 16 {
            panic!(
                "HMAC algorithm output size must be at least 16 (reset_token size): {}",
                type_name::<M>()
            );
        }

        Self {
            plaintext_buffer: Vec::new(),
            associated_data_buffer: Vec::new(),
            reserve_buffer: Vec::new(),
            aead,
            hmac,
        }
    }


    fn verify_retry_token(
        &mut self,
        peer_addr: IpAddr,
        peer_scid: &[u8],
        peer_dcid: &[u8],
        now: SystemTime,
    ) -> Result<Token, TokenError> {
        self.reserve_buffer.clear();
        self.write_retry_associated_data(peer_addr, peer_scid, peer_dcid);

        self.aead
            .unpack_and_decrypt(
                &self.associated_data_buffer,
                &mut self.plaintext_buffer,
                &mut self.reserve_buffer,
            )
            .map_err(|_| TokenError::Invalid)?;

        self.extract_retry_plaintext_data(now)
    }

    fn verify_identity_token(
        &mut self,
        peer_addr: IpAddr,
        now: SystemTime,
    ) -> Result<Token, TokenError> {
        self.reserve_buffer.clear();
        self.write_identity_associated_data(peer_addr);

        self.aead
            .unpack_and_decrypt(
                &self.associated_data_buffer,
                &mut self.plaintext_buffer,
                &mut self.reserve_buffer,
            )
            .map_err(|_| TokenError::Invalid)?;

        self.extract_identity_plaintext_data(now)
    }


    fn write_retry_associated_data(&mut self, peer_addr: IpAddr, scid: &[u8], dcid: &[u8]) {
        let buf = &mut self.associated_data_buffer;
        buf.clear();

        write_kind(TokenKind::Retry, buf);

        write_ip_addr(peer_addr, buf);
        write_padding((16 - length_of_ip_addr(peer_addr)) as u8, false, buf);

        write_cid(scid, buf);
        write_padding((MAX_CID_LEN - scid.len()) as u8, false, buf);

        write_cid(dcid, buf);
        // No need for padding.
        //
        // Padding is used here to not let the attacker to do something like this:
        // - token data: [scid: empty, dcid: 12345]
        // - QUIC packet: [scid: 123, dcid: 45]
    }

    fn write_identity_associated_data(&mut self, peer_addr: IpAddr) {
        let buf = &mut self.associated_data_buffer;
        buf.clear();

        write_kind(TokenKind::Identity, buf);
        write_ip_addr(peer_addr, buf);
    }


    fn write_retry_plaintext_data(&mut self, original_peer_dcid: &[u8], expires_at: SystemTime) {
        let buf = &mut self.plaintext_buffer;
        buf.clear();

        write_cid(original_peer_dcid, buf);
        write_padding((MAX_CID_LEN - original_peer_dcid.len()) as u8, true, buf);

        write_millis(to_millis(expires_at).unwrap_or(0), buf);
    }

    fn write_identity_plaintext_data(&mut self, expires_at: SystemTime) {
        let buf = &mut self.plaintext_buffer;
        buf.clear();

        write_padding(MAX_CID_LEN as u8, true, buf);
        write_millis(to_millis(expires_at).unwrap_or(0), buf);
    }


    fn extract_retry_plaintext_data(&mut self, now: SystemTime) -> Result<Token, TokenError> {
        let buf = &mut self.plaintext_buffer;

        let expires_at = to_systime(drain_millis(buf)?);
        if expires_at < now {
            return Err(TokenError::Expired(TokenKind::Retry, expires_at));
        }

        drain_padding(buf)?;
        let Some(peer_original_dcid) = ConnectionId::try_from_iter(buf.drain(..)) else {
            return Err(TokenError::Invalid);
        };

        Ok(Token::Retry {
            peer_original_dcid,
            expires_at,
        })
    }

    fn extract_identity_plaintext_data(&mut self, now: SystemTime) -> Result<Token, TokenError> {
        let buf = &mut self.plaintext_buffer;

        let expires_at = to_systime(drain_millis(buf)?);
        if expires_at < now {
            return Err(TokenError::Expired(TokenKind::Identity, expires_at));
        }

        drain_padding(buf)?;
        if !buf.is_empty() {
            return Err(TokenError::Invalid);
        }

        Ok(Token::Identity { expires_at })
    }
}

impl<A, M, AKeys, MKeys> TokenGenerator for PaddedTokenGenerator<A, M, AKeys, MKeys>
where
    A: aead::KeyInit + aead::AeadInPlace,
    M: digest::KeyInit + digest::Mac,
    AKeys: KeyConsumer,
    MKeys: KeyConsumer,
{
    fn generate_retry_token(
        &mut self,
        peer_addr: IpAddr,
        peer_scid: &[u8],
        peer_dcid: &[u8],
        server_new_scid: &[u8],
        expires_at: SystemTime,
    ) -> Option<&[u8]> {
        if peer_scid.len() > MAX_CID_LEN
            || peer_dcid.len() > MAX_CID_LEN
            || server_new_scid.len() > MAX_CID_LEN
        {
            return None;
        }

        self.write_retry_associated_data(peer_addr, peer_scid, server_new_scid);
        self.write_retry_plaintext_data(peer_dcid, expires_at);

        self.aead
            .encrypt_and_pack(&self.associated_data_buffer, &mut self.plaintext_buffer);

        Some(&self.plaintext_buffer)
    }

    fn generate_identity_token(&mut self, peer_addr: IpAddr, expires_at: SystemTime) -> &[u8] {
        self.write_identity_associated_data(peer_addr);
        self.write_identity_plaintext_data(expires_at);

        self.aead
            .encrypt_and_pack(&self.associated_data_buffer, &mut self.plaintext_buffer);

        &self.plaintext_buffer
    }

    fn generate_reset_token(&mut self, server_scid: &[u8]) -> u128 {
        self.plaintext_buffer.clear();

        // HMAC size is guaranteed to be > 16.
        // See `new()`.
        self.hmac.compute(server_scid, &mut self.plaintext_buffer);
        let array: [u8; 16] = self.plaintext_buffer[0..16].try_into().unwrap();
        u128::from_be_bytes(array)
    }


    fn verify_initial_token(
        &mut self,
        peer_addr: IpAddr,
        peer_scid: &[u8],
        peer_dcid: &[u8],
        token: &[u8],
        now: SystemTime,
    ) -> Result<Token, TokenError> {
        if token.is_empty() {
            return Err(TokenError::Invalid);
        }
        if peer_scid.len() > MAX_CID_LEN || peer_dcid.len() > MAX_CID_LEN {
            return Err(TokenError::Invalid);
        }

        self.plaintext_buffer.clear();
        self.plaintext_buffer.extend_from_slice(token);

        {
            let result = self.verify_identity_token(peer_addr, now);
            match &result {
                Ok(_) | Err(TokenError::Expired(_, _)) => {
                    return result;
                }
                Err(TokenError::Invalid) => {
                    // Continue...
                }
            }
        }

        self.plaintext_buffer.clear();
        self.plaintext_buffer.extend_from_slice(token);

        self.verify_retry_token(peer_addr, peer_scid, peer_dcid, now)
    }

    fn verify_reset_token(&mut self, server_scid: &[u8], token: u128) -> Result<(), TokenError> {
        match self
            .hmac
            .verify_fn(server_scid, &token.to_be_bytes(), |hmac| &hmac[0..16])
        {
            true => Ok(()),
            false => Err(TokenError::Invalid),
        }
    }
}


fn write_ip_addr(ip: IpAddr, out: &mut Vec<u8>) {
    match ip {
        IpAddr::V4(v4) => out.extend_from_slice(&v4.octets()),
        IpAddr::V6(v6) => out.extend_from_slice(&v6.octets()),
    };
}

fn write_cid(id: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(id);
}

fn write_padding(length: u8, random: bool, out: &mut Vec<u8>) {
    out.extend((0..length).map(|_| 0));

    if random {
        let range = (out.len() - length as usize)..;
        rng().fill_bytes(&mut out[range]);
    }

    out.extend_from_slice(&length.to_be_bytes());
}

fn write_millis(millis: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(&millis.to_be_bytes());
}

fn write_kind(kind: TokenKind, out: &mut Vec<u8>) {
    let byte = match kind {
        TokenKind::Retry => 0,
        TokenKind::Identity => 255,
    };

    out.push(byte);
}


fn drain_millis(token: &mut Vec<u8>) -> Result<u64, TokenError> {
    if token.len() < 8 {
        return Err(TokenError::Invalid);
    }

    let mut array = [0u8; 8];

    for i in (0..8).rev() {
        array[i] = token.pop().unwrap();
    }

    Ok(u64::from_be_bytes(array))
}

fn drain_padding(token: &mut Vec<u8>) -> Result<(), TokenError> {
    let Some(length) = token.pop() else {
        return Err(TokenError::Invalid);
    };
    if token.len() < length as usize {
        return Err(TokenError::Invalid);
    }

    for _ in 0..length {
        let _ = token.pop();
    }

    Ok(())
}


fn to_millis(sys_time: SystemTime) -> Option<u64> {
    let millis = sys_time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(millis).ok()
}

fn to_systime(timestamp: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(timestamp)
}

fn length_of_ip_addr(addr: IpAddr) -> usize {
    match addr {
        IpAddr::V4(v4) => v4.octets().len(),
        IpAddr::V6(v6) => v6.octets().len(),
    }
}
