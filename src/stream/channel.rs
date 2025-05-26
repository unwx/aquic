use crate::ApplicationError;
use crate::sync::{recv_init, rendezvous, send_once};
use std::fmt::{Display, Formatter};
use tokio::select;
use tokio::sync::watch;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum RecvError<E> {
    // TODO(docs):
    //  Internal Error / Connection close...
    HangUp,
    Finish,
    ResetStream(E),
    StreamReader(E),
}

impl<E: Display> Display for RecvError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::HangUp => write!(f, "Hang up"),
            RecvError::Finish => write!(f, "FIN"),
            RecvError::ResetStream(e) => write!(f, "RESET_STREAM({e})"),
            RecvError::StreamReader(e) => write!(f, "StreamReader({e})"),
        }
    }
}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum SendError<E> {
    HangUp,
    StopSending(E),
    StreamWriter(E),
}

impl<E: Display> Display for SendError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::HangUp => write!(f, "Hang up"),
            SendError::StopSending(e) => write!(f, "STOP_SENDING({e})"),
            SendError::StreamWriter(e) => write!(f, "StreamWriter({e})"),
        }
    }
}


#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct StreamPriority {
    pub urgency: u8,
    pub incremental: bool,
}

impl StreamPriority {
    fn new(urgency: u8, incremental: bool) -> Self {
        Self {
            urgency,
            incremental,
        }
    }
}


pub(crate) fn channel<T, E: ApplicationError>() -> (StreamSender<T, E>, StreamReceiver<T, E>) {
    let (message_sender, message_receiver) = rendezvous::channel();
    let (sender_error_sender, sender_error_receiver) = watch::channel(None);
    let (receiver_error_sender, receiver_error_receiver) = watch::channel(None);
    let (priority_sender, priority_receiver) = watch::channel(None);

    let stream_sender = StreamSender {
        message_sender,
        direction_error_sender: receiver_error_sender,
        direction_error_receiver: sender_error_receiver,
        priority_sender,
        error: None,
    };
    let stream_receiver = StreamReceiver {
        message_receiver,
        direction_error_sender: sender_error_sender,
        direction_error_receiver: receiver_error_receiver,
        priority_receiver,
        error: None,
    };

    (stream_sender, stream_receiver)
}


pub struct StreamSender<T, E> {
    message_sender: rendezvous::Sender<(Option<T>, bool)>,

    direction_error_sender: watch::Sender<Option<RecvError<E>>>,
    direction_error_receiver: watch::Receiver<Option<SendError<E>>>,

    priority_sender: watch::Sender<Option<StreamPriority>>,
    error: Option<SendError<E>>,
}

impl<T, E: ApplicationError> StreamSender<T, E> {
    pub async fn send(&mut self, value: T) -> Result<(), SendError<E>> {
        self.send_internal(Some(value), false).await
    }

    pub async fn send_and_finish(mut self, value: T) -> Result<(), SendError<E>> {
        self.send_and_finish_keep(value).await
    }

    pub async fn finish(mut self) -> Result<(), SendError<E>> {
        self.finish_keep().await
    }

    pub async fn send_and_finish_keep(&mut self, value: T) -> Result<(), SendError<E>> {
        self.send_internal(Some(value), true).await
    }

    pub async fn finish_keep(&mut self) -> Result<(), SendError<E>> {
        self.send_internal(None, true).await
    }


    pub fn reset_stream(mut self, code: E) {
        self.reset_stream_keep(code);
    }

    pub(crate) fn stream_reader_error(mut self, code: E) {
        self.stream_reader_error_keep(code);
    }

    pub fn reset_stream_keep(&mut self, code: E) {
        if self.error.is_none() {
            send_once(
                &mut self.direction_error_sender,
                RecvError::ResetStream(code),
            );

            self.close_with_error(SendError::HangUp);
        }
    }

    pub(crate) fn stream_reader_error_keep(&mut self, code: E) {
        if self.error.is_none() {
            send_once(
                &mut self.direction_error_sender,
                RecvError::StreamReader(code),
            );

            self.close_with_error(SendError::HangUp);
        }
    }


    // TODO(docs): not cancel-safe: may panic on attempt to send a message after a cancellation.
    //  Please send 'reset_stream' after a cancellation.
    async fn send_internal(&mut self, value: Option<T>, finish: bool) -> Result<(), SendError<E>> {
        if let Some(e) = self.error {
            return Err(e);
        }

        select! {
            biased;

            maybe_error = recv_init(&mut self.direction_error_receiver) => {
                self.close_with_error(maybe_error.unwrap_or(SendError::HangUp));
                Err(self.error().unwrap())
            },
            sent = self.message_sender.send((value, finish)) => {
                if sent {
                    if finish {
                        self.close_with_error(SendError::HangUp);
                    }

                    Ok(())
                } else {
                    self.close_with_error(SendError::HangUp);
                    Err(self.error().unwrap())
                }
            },
        }
    }


    pub fn error(&self) -> Option<SendError<E>> {
        self.error
    }

    // TODO(docs): cancel-safe
    pub async fn await_error(&mut self) -> SendError<E> {
        if let Some(e) = self.error {
            return e;
        }

        self.error = Some(
            recv_init(&mut self.direction_error_receiver)
                .await
                .unwrap_or(SendError::HangUp),
        );
        self.error.unwrap()
    }

    pub fn error_receiver(&self) -> watch::Receiver<Option<SendError<E>>> {
        self.direction_error_receiver.clone()
    }


    pub fn set_priority(&self, urgency: u8, incremental: bool) {
        self.priority_sender
            .send_replace(Some(StreamPriority::new(urgency, incremental)));
    }

    fn close_with_error(&mut self, error: SendError<E>) {
        debug_assert!(self.error.is_none());
        self.error = Some(error);
        self.message_sender.close();
    }
}

impl<T, E> Drop for StreamSender<T, E> {
    fn drop(&mut self) {
        if self.error.is_none() {
            send_once(&self.direction_error_sender, RecvError::HangUp);
        }
    }
}


pub struct StreamReceiver<T, E: ApplicationError> {
    message_receiver: rendezvous::Receiver<(Option<T>, bool)>,

    direction_error_sender: watch::Sender<Option<SendError<E>>>,
    direction_error_receiver: watch::Receiver<Option<RecvError<E>>>,

    priority_receiver: watch::Receiver<Option<StreamPriority>>,
    error: Option<RecvError<E>>,
}

impl<T, E: ApplicationError> StreamReceiver<T, E> {
    pub async fn recv(&mut self) -> Result<T, RecvError<E>> {
        match self.recv_full().await? {
            (Some(value), _) => Ok(value),
            (None, true) => Err(RecvError::Finish),
            (None, false) => {
                unreachable!("received (None, false) which should never be sent by 'StreamSender'");
            }
        }
    }

    // TODO(docs): cancel-safe
    pub async fn recv_full(&mut self) -> Result<(Option<T>, bool), RecvError<E>> {
        if let Some(e) = self.error {
            return Err(e);
        }

        let mut result = select! {
            biased;

            maybe_error = recv_init(&mut self.direction_error_receiver) => {
                self.close_with_error(maybe_error.unwrap_or(RecvError::HangUp));
                Err(self.error.unwrap())
            },
            result = self.message_receiver.recv() => {
                result.ok_or(RecvError::HangUp)
            },
        };

        if let Ok((message, finish)) = &result {
            assert!(
                message.is_some() || *finish,
                "received (None, false) which should never be sent by 'StreamSender'"
            );

            if message.is_none() && *finish {
                result = Err(RecvError::Finish);
            }
        }

        if let &Err(e) = &result {
            self.close_with_error(e);
        }

        result
    }


    pub fn stop_sending(mut self, code: E) {
        self.stop_sending_keep(code);
    }

    pub fn stream_writer_error(mut self, code: E) {
        self.stream_writer_error_keep(code);
    }

    pub fn stop_sending_keep(&mut self, code: E) {
        if self.error.is_none() {
            send_once(
                &mut self.direction_error_sender,
                SendError::StopSending(code),
            );

            self.close_with_error(RecvError::HangUp);
        }
    }

    pub(crate) fn stream_writer_error_keep(&mut self, code: E) {
        if self.error.is_none() {
            send_once(
                &mut self.direction_error_sender,
                SendError::StreamWriter(code),
            );

            self.close_with_error(RecvError::HangUp);
        }
    }


    pub(crate) fn priority_once(&mut self) -> Option<StreamPriority> {
        self.priority_receiver.borrow_and_update().as_ref().copied()
    }

    fn close_with_error(&mut self, error: RecvError<E>) {
        debug_assert!(self.error.is_none());
        self.message_receiver.close();
        self.error = Some(error);
    }
}
