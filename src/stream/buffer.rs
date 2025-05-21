use bytes::Bytes;
use quiche::{BufFactory, BufSplit, Connection, Error};
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct StreamBuffer {
    stream_id: u64,
    deque: VecDeque<Bytes>,
    length: usize,
    finish: bool,
}

impl StreamBuffer {
    pub fn new(stream_id: u64) -> Self {
        Self {
            stream_id,
            deque: VecDeque::new(),
            length: 0,
            finish: false,
        }
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn is_finished(&self) -> bool {
        self.finish
    }

    pub fn finish(&mut self) {
        self.finish = true;
    }


    pub fn append(&mut self, bytes: Bytes) -> bool {
        if self.finish {
            return false;
        }

        self.push_back(bytes);
        true
    }

    pub fn drain<BF>(&mut self, connection: &mut Connection<BF>) -> Result<bool, Error>
    where
        BF: BufFactory,
        BF::Buf: BufSplit + From<Bytes> + Into<Bytes>,
    {
        let length = self.length;
        while let Some(bytes) = self.deque.pop_front() {
            match connection.stream_send_zc(
                self.stream_id,
                BF::Buf::from(bytes.clone()),
                Some(bytes.len()),
                self.finish && self.deque.is_empty(),
            ) {
                Ok((_, None)) => {
                    continue;
                }
                Ok((_, Some(bytes_left))) => {
                    self.push_front(bytes_left.into());
                    break;
                }

                Err(Error::Done) => {
                    self.push_front(bytes);
                    break;
                }
                Err(e) => {
                    self.push_front(bytes);
                    return Err(e);
                }
            }
        }

        Ok(length != self.length)
    }


    fn push_front(&mut self, bytes: Bytes) {
        self.length += bytes.len();
        self.deque.push_front(bytes);
    }

    fn push_back(&mut self, bytes: Bytes) {
        self.length += bytes.len();
        self.deque.push_back(bytes);
    }

    fn pop_front(&mut self) -> Option<Bytes> {
        self
            .deque
            .pop_front()
            .inspect(|it| self.length -= it.len())
    }
}
