use crate::stream::StreamType::{ClientBidi, ClientUni, ServerBidi, ServerUni};

pub mod mapper;
pub mod channel;


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) enum StreamType {
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


pub(crate) fn client_bidi_id_for_count(count: u64) -> u64 {
    count * 4
}

pub(crate) fn server_bidi_id_for_count(count: u64) -> u64 {
    (count * 4) + 1
}

pub(crate) fn client_uni_id_for_count(count: u64) -> u64 {
    (count * 4) + 2
}

pub(crate) fn server_uni_id_for_count(count: u64) -> u64 {
    (count * 4) + 3
}
