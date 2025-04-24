use crate::common::StreamType::{ClientBidi, ClientUni, ServerBidi, ServerUni};
use bytes::Bytes;
use quiche::{BufFactory, BufSplit};
use std::fmt::Debug;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum StreamType {
    ClientBidi,
    ServerBidi,
    ClientUni,
    ServerUni,
}

impl StreamType {
    pub fn is_client(self) -> bool {
        self == ClientBidi || self == ClientUni
    }

    pub fn is_server(self) -> bool {
        !self.is_client()
    }

    pub fn is_bidi(self) -> bool {
        self == ClientBidi || self == ServerBidi
    }

    pub fn is_uni(self) -> bool {
        !self.is_bidi()
    }
}

impl From<u64> for StreamType {
    fn from(id: u64) -> Self {
        match id % 4 {
            0 => ClientBidi,
            1 => ServerBidi,
            2 => ClientUni,
            3 => ServerUni,
            _ => unreachable!(),
        }
    }
}


pub fn client_bidi_id_for_count(count: u64) -> u64 {
    count * 4
}

pub fn server_bidi_id_for_count(count: u64) -> u64 {
    (count * 4) + 1
}

pub fn client_uni_id_for_count(count: u64) -> u64 {
    (count * 4) + 2
}

pub fn server_uni_id_for_count(count: u64) -> u64 {
    (count * 4) + 3
}


#[derive(Debug, Clone)]
pub struct BufView {
    bytes: Bytes,
}

impl From<Bytes> for BufView {
    fn from(bytes: Bytes) -> Self {
        Self { bytes }
    }
}

impl AsRef<[u8]> for BufView {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl BufSplit for BufView {
    fn split_at(&mut self, at: usize) -> Self {
        BufView::from(self.bytes.split_off(at))
    }
}


#[derive(Debug, Copy, Clone, Default)]
pub struct BufViewFactory;

impl BufFactory for BufViewFactory {
    type Buf = BufView;

    fn buf_from_slice(buf: &[u8]) -> Self::Buf {
        BufView::from(Bytes::copy_from_slice(buf))
    }
}
