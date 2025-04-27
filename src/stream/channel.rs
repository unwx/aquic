use crate::future::state::Switch;
use std::any::type_name;
use std::fmt::{Display, Formatter};
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ReadError {
    Fin,
    ResetStream(u64),
    ConnectionClose,
}

impl Display for ReadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Fin => write!(f, "FIN"),
            ReadError::ResetStream(code) => write!(f, "RESET_STREAM({code})"),
            ReadError::ConnectionClose => write!(f, "Connection closed"),
        }
    }
}


#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum WriteError {
    HangUp,
    StopSending(u64),
    ResetStream(u64),
    ConnectionClose,
}

impl Display for WriteError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::HangUp => write!(f, "Hang up"),
            WriteError::StopSending(code) => write!(f, "STOP_SENDING({code})"),
            WriteError::ResetStream(code) => write!(f, "RESET_STREAM({code})"),
            WriteError::ConnectionClose => write!(f, "Connection closed"),
        }
    }
}


pub(crate) fn channel<T>(
    buffer: usize,
    stream_switch: Switch<u64>,
    connection_switch: Switch<()>,
    default_stop_sending_code: Option<u64>,
) -> (StreamSender<T>, StreamReceiver<T>) {
    let (sender, receiver) = mpsc::channel(buffer);
    let direction_switch = Switch::new();

    let stream_sender = StreamSender::new(
        sender,
        direction_switch.clone(),
        stream_switch.clone(),
        connection_switch.clone(),
    );
    let stream_receiver = StreamReceiver::new(
        receiver,
        direction_switch.clone(),
        stream_switch.clone(),
        connection_switch.clone(),
        default_stop_sending_code,
    );

    (stream_sender, stream_receiver)
}


#[derive(Debug, Clone)]
pub struct StreamSender<T> {
    sender: Sender<T>,

    direction_switch: Switch<u64>,
    stream_switch: Switch<u64>,
    connection_switch: Switch<()>,

    error: Option<WriteError>,
}

impl<T> StreamSender<T> {
    pub(crate) fn new(
        sender: Sender<T>,
        direction_switch: Switch<u64>,
        stream_switch: Switch<u64>,
        connection_switch: Switch<()>,
    ) -> Self {
        Self {
            sender,
            direction_switch,
            stream_switch,
            connection_switch,
            error: None,
        }
    }


    pub async fn send(&mut self, value: T) -> Result<(), WriteError> {
        if let Some(error) = self.error {
            return Err(error);
        }

        enum Action {
            Send(bool),
            StopSending(u64),
            ResetStream(u64),
            ConnectionClose,
        }

        let action = select! {
            biased;

            // TODO(performance):
            //  Too many polls, will moving it to the FuturesUnordered help?
            a = self.connection_switch.switched() => {
                Action::ConnectionClose
            }
            a = self.stream_switch.switched() => {
                Action::ResetStream(a)
            }
            a = self.direction_switch.switched() => {
                Action::StopSending(a)
            }
            a = self.sender.send(value) => {
                Action::Send(a.is_ok())
            }
        };

        let error = match action {
            Action::Send(success) => {
                if success {
                    None
                } else {
                    Some(WriteError::HangUp)
                }
            }
            Action::StopSending(code) => Some(WriteError::StopSending(code)),
            Action::ResetStream(code) => Some(WriteError::ResetStream(code)),
            Action::ConnectionClose => Some(WriteError::ConnectionClose),
        };

        if let Some(error) = error {
            self.error = Some(error);
            Err(error)
        } else {
            Ok(())
        }
    }

    pub fn reset_stream(self, code: u64) {
        let _ = self.stream_switch.try_switch(code);
    }


    pub(crate) fn direction_switch(&self) -> Switch<u64> {
        self.direction_switch.clone()
    }

    pub(crate) fn stream_switch(&self) -> Switch<u64> {
        self.stream_switch.clone()
    }

    pub(crate) fn connection_switch(&self) -> Switch<()> {
        self.connection_switch.clone()
    }
}


#[derive(Debug)]
pub struct StreamReceiver<T> {
    receiver: Receiver<T>,

    direction_switch: Switch<u64>,
    stream_switch: Switch<u64>,
    connection_switch: Switch<()>,

    default_stop_sending_code: Option<u64>,
    error: Option<ReadError>,
}

impl<T> StreamReceiver<T> {
    pub(crate) fn new(
        receiver: Receiver<T>,
        direction_switch: Switch<u64>,
        stream_switch: Switch<u64>,
        connection_switch: Switch<()>,
        default_stop_sending_code: Option<u64>,
    ) -> Self {
        Self {
            receiver,
            direction_switch,
            stream_switch,
            connection_switch,
            default_stop_sending_code,
            error: None,
        }
    }


    pub async fn recv(&mut self) -> Result<T, ReadError> {
        if let Some(error) = self.error {
            if error == ReadError::Fin {
                return Err(error);
            }

            if self.receiver.sender_strong_count() == 0 && self.receiver.is_empty() {
                return Err(error);
            }
        }

        enum Action<T> {
            Receive(Option<T>),
            ResetStream(u64),
            ConnectionClose,
        }

        let action = {
            if self.error.is_some() {
                Action::Receive(self.receiver.recv().await)
            } else {
                select! {
                    biased;

                    // TODO(performance):
                    //  Too many polls, will moving it to the FuturesUnordered help?
                    a = self.connection_switch.switched() => {
                        Action::ConnectionClose,
                    }
                    a = self.stream_switch.switched() => {
                        Action::ResetStream(a)
                    }
                    a = self.receiver.recv() => {
                        Action::Receive(a)
                    }
                }
            }
        };

        let result = match action {
            Action::Receive(message) => {
                if let Some(message) = message {
                    Ok(message)
                } else {
                    Err(ReadError::Fin)
                }
            }
            Action::ResetStream(code) => Err(ReadError::ResetStream(code)),
            Action::ConnectionClose => Err(ReadError::ConnectionClose),
        };

        result.map_err(|error| {
            if let Some(existing_error) = self.error {
                existing_error
            } else {
                self.error = Some(error);
                error
            }
        })
    }

    pub fn stop_sending(self, code: u64) {
        let _ = self.direction_switch.try_switch(code);
    }


    pub(crate) fn direction_switch(&self) -> Switch<u64> {
        self.direction_switch.clone()
    }

    pub(crate) fn stream_switch(&self) -> Switch<u64> {
        self.stream_switch.clone()
    }

    pub(crate) fn connection_switch(&self) -> Switch<()> {
        self.connection_switch.clone()
    }
}

impl<T> Drop for StreamReceiver<T> {
    fn drop(&mut self) {
        if self.error.is_none()
            && !self.connection_switch.is_switched()
            && !self.stream_switch.is_switched()
            && !self.direction_switch.is_switched()
        {
            if let Some(default_code) = self.default_stop_sending_code {
                let _ = self.direction_switch.try_switch(default_code);
            } else {
                panic!(
                    "attempt to drop StreamReceiver<{}> without invoking 'stop_sending()' first, \
                    while 'default_stop_sending_code' is None",
                    type_name::<T>()
                )
            }
        }
    }
}
