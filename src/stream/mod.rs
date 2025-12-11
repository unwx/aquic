use crate::stream::StreamType::{ClientBidi, ClientUni, ServerBidi, ServerUni};
use quiche::{BufFactory, BufSplit, Connection, Shutdown};

mod buffer;
mod codec;
mod direction;
mod sync;

pub use sync::*;

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


struct ReadStream<'a, BF: BufFactory> {
    id: u64,
    connection: &'a mut Connection<BF>,
}

impl<'a, BF: BufFactory> ReadStream<'a, BF> {
    fn new(id: u64, connection: &'a mut Connection<BF>) -> Self {
        Self { id, connection }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn recv(&mut self, buffer: &mut [u8]) -> quiche::Result<(usize, bool)> {
        self.connection.stream_recv(self.id, buffer)
    }

    #[rustfmt::skip]
    //noinspection DuplicatedCode
    pub fn shutdown<E: Into<u64>>(&mut self, error: E) -> quiche::Result<()> {
        self.connection.stream_shutdown(self.id, Shutdown::Read, error.into())
    }
}


struct WriteStream<'a, BF: BufFactory> {
    id: u64,
    connection: &'a mut Connection<BF>,
}

impl<'a, BF: BufFactory> WriteStream<'a, BF> {
    fn new(id: u64, connection: &'a mut Connection<BF>) -> Self {
        Self { id, connection }
    }

    pub fn id(&self) -> u64 {
        self.id
    }


    pub fn priority(&mut self, urgency: u8, incremental: bool) -> quiche::Result<()> {
        self.connection
            .stream_priority(self.id, urgency, incremental)
    }

    #[rustfmt::skip]
    //noinspection DuplicatedCode
    pub fn shutdown<E: Into<u64>>(&mut self, error: E) -> quiche::Result<()> {
        self.connection.stream_shutdown(self.id, Shutdown::Write, error.into())
    }
}

impl<'a, BF> WriteStream<'a, BF>
where
    BF: BufFactory,
    BF::Buf: BufSplit,
{
    #[rustfmt::skip]
    pub fn send(
        &mut self,
        buffer: BF::Buf,
        length: Option<usize>,
        finish: bool,
    ) -> quiche::Result<(usize, Option<BF::Buf>)> {
        self.connection.stream_send_zc(self.id, buffer, length, finish)
    }
}
