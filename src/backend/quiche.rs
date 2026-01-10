use crate::backend::Backend;
use crate::backend::stream::{StreamBackend, StreamError, StreamResult};
use crate::stream::{Priority, StreamId};
use bytes::Bytes;
use quiche::{BufFactory, BufSplit, Connection, Error, Shutdown};

impl From<Error> for StreamError {
    fn from(value: Error) -> Self {
        match value {
            Error::StreamStopped(e) => Self::StopSending(e),
            Error::StreamReset(e) => Self::ResetSending(e),
            other => Self::Other(format!("{}", other).into()),
        }
    }
}


pub(crate) struct Quiche {
    connection: Connection<BytesFactory>,
}

impl Backend for Quiche {}


impl Quiche {
    fn stream_shutdown(
        &mut self,
        stream_id: StreamId,
        direction: Shutdown,
        err: u64,
    ) -> StreamResult<()> {
        match self.connection.stream_shutdown(stream_id, direction, err) {
            Ok(_) => Ok(()),
            Err(Error::Done) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }
}

impl StreamBackend for Quiche {
    #[rustfmt::skip]
    fn stream_recv(&mut self, stream_id: StreamId, out: &mut [u8]) -> StreamResult<(usize, bool)> {
        match self.connection.stream_recv(stream_id, out) {
            Ok((read, fin)) => {
                Ok((read, fin))
            },
            Err(Error::Done) => {
                if self.connection.stream_finished(stream_id) {
                    return Err(StreamError::Finish);
                }

                Ok((0, false))
            }
            Err(other) => {
                Err(other.into())
            },
        }
    }

    #[rustfmt::skip]
    fn stream_send(
        &mut self,
        stream_id: StreamId,
        bytes: Bytes,
        fin: bool,
    ) -> StreamResult<Option<Bytes>> {
        let length = bytes.len();
        if length == 0 && !fin {
            return Ok(None);
        }

        match self.connection.stream_send_zc(stream_id, bytes.into(), Some(length), fin) {
            Ok((_, left)) => {
                Ok(left.map(Into::<Bytes>::into).filter(|it| it.len() > 0))
            },
            Err(Error::Done) => {
                if self.connection.stream_finished(stream_id) {
                    return Err(StreamError::Finish);
                }

                Ok(None)
            },
            Err(Error::FinalSize) => {
                Err(StreamError::Finish)
            },
            Err(other) => {
                Err(other.into())
            },
        }
    }

    fn stream_priority(&mut self, stream_id: StreamId, priority: Priority) -> StreamResult<()> {
        self.connection
            .stream_priority(stream_id, priority.urgency, priority.incremental)
            .map_err(|e| e.into())
    }


    fn stream_readable_next(&mut self) -> Option<StreamId> {
        self.connection.stream_readable_next()
    }

    fn stream_writable_next(&mut self) -> Option<StreamId> {
        self.connection.stream_writable_next()
    }


    fn stream_stop_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()> {
        self.stream_shutdown(stream_id, Shutdown::Read, err)
    }

    fn stream_reset_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()> {
        self.stream_shutdown(stream_id, Shutdown::Write, err)
    }
}


/// [Bytes] wrapper to be used for [BufFactory].
#[repr(transparent)]
#[derive(Debug, Clone)]
struct View(Bytes);

impl From<Bytes> for View {
    fn from(bytes: Bytes) -> Self {
        Self(bytes)
    }
}

impl Into<Bytes> for View {
    fn into(self) -> Bytes {
        self.0
    }
}

impl AsRef<[u8]> for View {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl BufSplit for View {
    fn split_at(&mut self, at: usize) -> Self {
        View::from(self.0.split_off(at))
    }
}


/// [Bytes] view wrapper to be used as [BufFactory],
/// instead of [quiche::range_buf::DefaultBufFactory].
#[derive(Debug, Copy, Clone, Default)]
struct BytesFactory;

impl BufFactory for BytesFactory {
    type Buf = View;

    fn buf_from_slice(buf: &[u8]) -> Self::Buf {
        View::from(Bytes::copy_from_slice(buf))
    }
}
