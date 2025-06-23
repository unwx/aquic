use crate::buffer::BufViewFactory;
use crate::stream::WriteStream;
use bytes::Bytes;
use quiche::Error;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct StreamBuffer {
    deque: VecDeque<Bytes>,
    finish: bool,
}

impl StreamBuffer {
    pub fn new() -> Self {
        Self {
            deque: VecDeque::new(),
            finish: false,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finish
    }

    pub fn finish(&mut self) {
        self.finish = true;
    }

    pub fn is_empty(&self) -> bool {
        self.deque.is_empty()
    }


    pub fn append(&mut self, bytes: Bytes) -> bool {
        if self.finish {
            return false;
        }

        self.deque.push_back(bytes);
        true
    }

    pub fn drain(&mut self, stream: &mut WriteStream<BufViewFactory>) -> Result<bool, Error> {
        let mut sent_something = false;

        while let Some(bytes) = self.deque.pop_front() {
            match stream.send(
                bytes.clone().into(),
                Some(bytes.len()),
                self.finish && self.deque.is_empty(),
            ) {
                Ok((_, None)) => {
                    sent_something = true;
                    continue;
                }
                Ok((_, Some(bytes_left))) => {
                    let bytes_left: Bytes = bytes_left.into();
                    sent_something = sent_something || bytes.len() > bytes_left.len();

                    self.deque.push_front(bytes_left);
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

        Ok(sent_something)
    }
}
