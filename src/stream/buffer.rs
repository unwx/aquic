use bytes::Bytes;
use quiche::{BufFactory, BufSplit, Connection, Error};
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct StreamBuffer {
    stream_id: u64,
    deque: VecDeque<Bytes>,
    finish: bool,
}

impl StreamBuffer {
    pub fn new(stream_id: u64) -> Self {
        Self {
            stream_id,
            deque: VecDeque::new(),
            finish: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.deque.is_empty()
    }

    pub fn is_finished(&self) -> bool {
        self.finish
    }

    pub fn finish(&mut self) {
        self.finish = true;
    }


    pub fn append(&mut self, bytes: Bytes) -> bool {
        if self.finish {
            self.deque.push_back(bytes);
        }

        self.finish
    }

    pub fn drain<BF>(
        &mut self,
        connection: &mut Connection<BF>,
    ) -> Result<bool, Error>
    where
        BF: BufFactory,
        BF::Buf: BufSplit + From<Bytes> + Into<Bytes>,
    {
        let mut drain_something = false;

        while let Some(bytes) = self.deque.pop_front() {
            match connection.stream_send_zc(
                self.stream_id,
                BF::Buf::from(bytes.clone()),
                Some(bytes.len()),
                self.finish && self.deque.is_empty(),
            ) {
                Ok((_, None)) => {
                    drain_something = true;
                    continue;
                }
                Ok((_, Some(bytes_left))) => {
                    self.deque.push_front(bytes_left.into());
                    drain_something = true;
                    break;
                }

                Err(Error::Done) => {
                    self.deque.push_front(bytes);
                    break;
                }
                Err(e) => {
                    self.deque.push_front(bytes);
                    return Err(e);
                }
            }
        }

        Ok(drain_something)
    }
}
