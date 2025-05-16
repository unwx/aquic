use crate::ApplicationError;
use crate::future::state::{Observer, Switch};
use std::fmt::{Display, Formatter};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{mpsc, watch};

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


pub(crate) fn channel<T, E: ApplicationError>(
    capacity: usize,
) -> (StreamSender<T, E>, StreamReceiver<T, E>) {
    let (sender, receiver) = mpsc::channel(capacity);
    let (priority_sender, priority_receiver) = watch::channel(None);

    let sender_switch = Switch::new();
    let receiver_switch = Switch::new();

    let stream_sender = StreamSender::new(
        sender,
        receiver_switch.clone(),
        sender_switch.clone().into(),
        priority_sender,
    );
    let stream_receiver = StreamReceiver::new(
        receiver,
        sender_switch,
        receiver_switch.into(),
        priority_receiver,
    );

    (stream_sender, stream_receiver)
}


pub struct StreamSender<T, E: ApplicationError> {
    sender: Sender<(Option<T>, bool)>,

    receiver_error_switch: Switch<RecvError<E>>,
    sender_error_observer: Observer<Switch<SendError<E>>>,
    priority_sender: watch::Sender<Option<StreamPriority>>,

    error: Option<SendError<E>>,
}

impl<T, E: ApplicationError> StreamSender<T, E> {
    fn new(
        sender: Sender<(Option<T>, bool)>,
        receiver_error_switch: Switch<RecvError<E>>,
        sender_error_observer: Observer<Switch<SendError<E>>>,
        priority_sender: watch::Sender<Option<StreamPriority>>,
    ) -> Self {
        Self {
            sender,
            receiver_error_switch,
            sender_error_observer,
            priority_sender,
            error: None,
        }
    }


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
            let _ = self
                .receiver_error_switch
                .try_switch(RecvError::ResetStream(code));
        }
    }

    pub(crate) fn stream_reader_error_keep(&mut self, code: E) {
        if self.error.is_none() {
            let _ = self
                .receiver_error_switch
                .try_switch(RecvError::StreamReader(code));
        }
    }


    // TODO(docs): cancel-safe
    //  Therefore 'send_and_finish_keep()' and 'finish_keep()' cancel-safe.
    async fn send_internal(&mut self, value: Option<T>, finish: bool) -> Result<(), SendError<E>> {
        if let Some(e) = self.error {
            return Err(e);
        }

        select! {
            biased;

            error = self.sender_error_observer.switched() => {
                Err(error)
            },
            result = self.sender.send((value, finish)) => {
                result
                    .inspect(|_| {
                        if finish {
                            self.error = Some(SendError::HangUp);
                            // But result itself is 'Ok' right now.
                        }
                    })
                    .map_err(|_| SendError::HangUp)
                    .inspect_err(|&e| self.error = Some(e))
            },
        }
    }


    pub fn capacity(&self) -> usize {
        self.sender.capacity()
    }

    pub fn error(&self) -> Option<SendError<E>> {
        self.error
    }

    pub async fn error_waiting(&mut self) -> SendError<E> {
        if let Some(e) = self.error {
            return e;
        }

        self.error = Some(self.sender_error_observer.switched().await);
        self.error.unwrap()
    }

    // TODO(visibility): should be public?
    pub(crate) fn error_observer(&self) -> Observer<Switch<SendError<E>>> {
        self.sender_error_observer.clone()
    }


    pub fn set_priority(&self, urgency: u8, incremental: bool) {
        self.priority_sender
            .send_replace(Some(StreamPriority::new(urgency, incremental)));
    }
}

impl<T, E: ApplicationError> Drop for StreamSender<T, E> {
    fn drop(&mut self) {
        if self.error.is_none() {
            let _ = self.receiver_error_switch.try_switch(RecvError::HangUp);
        }
    }
}


pub struct StreamReceiver<T, E: ApplicationError> {
    receiver: Receiver<(Option<T>, bool)>,

    sender_error_switch: Switch<SendError<E>>,
    receiver_error_observer: Observer<Switch<RecvError<E>>>,
    priority_receiver: watch::Receiver<Option<StreamPriority>>,

    error: Option<RecvError<E>>,
}

impl<T, E: ApplicationError> StreamReceiver<T, E> {
    fn new(
        receiver: Receiver<(Option<T>, bool)>,
        sender_error_switch: Switch<SendError<E>>,
        receiver_error_observer: Observer<Switch<RecvError<E>>>,
        priority_receiver: watch::Receiver<Option<StreamPriority>>,
    ) -> Self {
        Self {
            receiver,
            sender_error_switch,
            receiver_error_observer,
            priority_receiver,
            error: None,
        }
    }


    pub async fn recv(&mut self) -> Result<T, RecvError<E>> {
        match self.recv_full().await? {
            (Some(value), _) => Ok(value),
            (None, true) => Err(RecvError::Finish),
            (None, false) => {
                unreachable!("received (None, false) which should never be sent by 'StreamSender'");
            }
        }
    }

    pub async fn recv_full(&mut self) -> Result<(Option<T>, bool), RecvError<E>> {
        if let Some(e) = self.error {
            return Err(e);
        }

        let mut result = select! {
            biased;

            error = self.receiver_error_observer.switched() => {
                Err(error)
            },
            result = self.receiver.recv() => {
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
            self.error = Some(e)
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
            let _ = self
                .sender_error_switch
                .try_switch(SendError::StopSending(code));
        }
    }

    pub(crate) fn stream_writer_error_keep(&mut self, code: E) {
        if self.error.is_none() {
            let _ = self
                .sender_error_switch
                .try_switch(SendError::StreamWriter(code));
        }
    }


    pub(crate) fn priority_once(&mut self) -> Option<StreamPriority> {
        self.priority_receiver.borrow_and_update().as_ref().copied()
    }
}
