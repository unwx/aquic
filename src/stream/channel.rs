use crate::future::state::{Switch, SwitchWatch};
use std::any::type_name;
use std::fmt::{Display, Formatter};
use std::thread;
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::warn;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ReadError {
    Finish,
    ResetStream(u64),
    ConnectionClose,
}

impl Display for ReadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Finish => write!(f, "FIN"),
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
    reset_stream_switch: Switch<u64>,
    connection_state_watch: SwitchWatch<()>,
    internal_error_code: Option<u64>,
) -> (StreamSender<T>, StreamReceiver<T>) {
    let (sender, receiver) = mpsc::channel(buffer);
    let stop_sending_switch = Switch::new();

    let stream_sender = StreamSender::new(
        sender,
        stop_sending_switch.clone(),
        reset_stream_switch.clone(),
        connection_state_watch.clone(),
        internal_error_code,
    );
    let stream_receiver = StreamReceiver::new(
        receiver,
        stop_sending_switch.clone(),
        reset_stream_switch.clone(),
        connection_state_watch.clone(),
        internal_error_code,
    );

    (stream_sender, stream_receiver)
}


#[derive(Debug)]
pub struct StreamSender<T> {
    sender: Sender<(T, bool)>,

    stop_sending_switch: Switch<u64>,
    reset_stream_switch: Switch<u64>,
    connection_state_watch: SwitchWatch<()>,

    internal_error_code: Option<u64>,
    error: Option<WriteError>,
}

impl<T> StreamSender<T> {
    pub(crate) fn new(
        sender: Sender<(T, bool)>,
        stop_sending_switch: Switch<u64>,
        reset_stream_switch: Switch<u64>,
        connection_state_watch: SwitchWatch<()>,
        internal_error_code: Option<u64>,
    ) -> Self {
        Self {
            sender,
            stop_sending_switch,
            reset_stream_switch,
            connection_state_watch,
            internal_error_code,
            error: None,
        }
    }


    pub async fn send(&mut self, value: T) -> Result<(), WriteError> {
        self.send_internal(value, false).await
    }

    pub async fn send_and_finish(mut self, value: T) -> Result<(), WriteError> {
        self.send_internal(value, true).await
    }

    async fn send_internal(
        &mut self,
        value: T,
        no_more_messages_hint: bool,
    ) -> Result<(), WriteError> {
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
            a = self.connection_state_watch.switched() => {
                Action::ConnectionClose
            }
            a = self.reset_stream_switch.switched() => {
                Action::ResetStream(a)
            }
            a = self.stop_sending_switch.switched() => {
                Action::StopSending(a)
            }
            a = self.sender.send((value, no_more_messages_hint)) => {
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


    pub fn finish(self) {
        drop(self)
    }

    pub fn reset_stream(self, code: u64) {
        let _ = self.reset_stream_switch.try_switch(code);
    }


    pub(crate) fn stop_sending_switch(&self) -> Switch<u64> {
        self.stop_sending_switch.clone()
    }

    pub(crate) fn reset_stream_switch(&self) -> Switch<u64> {
        self.reset_stream_switch.clone()
    }

    pub(crate) fn connection_state_watch(&self) -> SwitchWatch<()> {
        self.connection_state_watch.clone()
    }
}

impl<T> Drop for StreamSender<T> {
    fn drop(&mut self) {
        if thread::panicking()
            && self.error.is_none()
            && !self.connection_state_watch.is_switched()
            && !self.stop_sending_switch.is_switched()
        {
            if let Some(code) = self.internal_error_code {
                let _ = self.reset_stream_switch.try_switch(code);
            }
        }
    }
}


#[derive(Debug)]
pub struct StreamReceiver<T> {
    receiver: Receiver<(T, bool)>,

    stop_sending_switch: Switch<u64>,
    reset_stream_switch: Switch<u64>,
    connection_state_watch: SwitchWatch<()>,

    internal_error_code: Option<u64>,
    error: Option<ReadError>,
}

impl<T> StreamReceiver<T> {
    pub(crate) fn new(
        receiver: Receiver<(T, bool)>,
        stop_sending_switch: Switch<u64>,
        reset_stream_switch: Switch<u64>,
        connection_state_watch: SwitchWatch<()>,
        internal_error_code: Option<u64>,
    ) -> Self {
        Self {
            receiver,
            stop_sending_switch,
            reset_stream_switch,
            connection_state_watch,
            internal_error_code,
            error: None,
        }
    }


    pub async fn recv(&mut self) -> Result<T, ReadError> {
        self.recv_with_hint()
            .await
            .map(|(value, _no_more_messages_hint)| value)
    }

    pub async fn recv_with_hint(&mut self) -> Result<(T, bool), ReadError> {
        if let Some(error) = self.error {
            if error == ReadError::Finish {
                return Err(error);
            }

            if self.receiver.sender_strong_count() == 0 && self.receiver.is_empty() {
                return Err(error);
            }
        }

        enum Action<T> {
            Receive(Option<(T, bool)>),
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
                    a = self.connection_state_watch.switched() => {
                        Action::ConnectionClose
                    }
                    a = self.reset_stream_switch.switched() => {
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
                    Err(ReadError::Finish)
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
        let _ = self.stop_sending_switch.try_switch(code);
    }


    pub(crate) fn stop_sending_switch(&self) -> Switch<u64> {
        self.stop_sending_switch.clone()
    }

    pub(crate) fn reset_stream_switch(&self) -> Switch<u64> {
        self.reset_stream_switch.clone()
    }

    pub(crate) fn connection_state_watch(&self) -> SwitchWatch<()> {
        self.connection_state_watch.clone()
    }
}

impl<T> Drop for StreamReceiver<T> {
    fn drop(&mut self) {
        if self.error.is_none()
            && !self.connection_state_watch.is_switched()
            && !self.reset_stream_switch.is_switched()
        {
            if let Some(default_code) = self.internal_error_code {
                let _ = self.stop_sending_switch.try_switch(default_code);
            } else {
                if !self.stop_sending_switch.is_switched() {
                    warn!(
                        "detected 'StreamReceiver<{}>.drop()' without invoking 'stop_sending(code)' first, \
                        while 'internal_error_code' is None",
                        type_name::<T>()
                    )
                }
            }
        }
    }
}
