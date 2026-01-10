use crate::Spec;
use crate::backend::StreamError;
use crate::executor::Executor;
use crate::stream::shared::InStreamBackend;
use crate::stream::{Decoder, Error, Payload};
use crate::sync::oneshot;
use crate::sync::stream;
use std::any::type_name;
use tokio::select;
use tracing::{Instrument, Span, debug};

/// An incoming direction of QUIC stream,
/// acts like a bridge between protocol backend and application listener.
///
/// Its role is to read incoming stream data,
/// decode it, and send it via channel.
///
/// The entire flow is sequential: implementation won't read from the stream,
/// unless the listener is ready to receive the data.
pub(crate) struct Incoming<S: Spec> {
    /// An API to communicate with QUIC implementation.
    backend: InStreamBackend,

    /// Decoder for decoding raw bytes stream.
    decoder: S::Decoder,

    /// Channel for sending decoded items.
    item_sender: stream::Sender<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error<S>>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error<S>>,

    /// `true`, if 'FIN' was received from the stream.
    draining: bool,
}

impl<S: Spec> Incoming<S> {
    pub fn new(
        backend: InStreamBackend,
        decoder: S::Decoder,
        item_sender: stream::Sender<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        Self {
            backend,
            decoder,
            item_sender,
            cancel_sender,
            cancel_receiver,
            draining: false,
        }
    }

    /// Starts the I/O loop in a separate task: reads data from the stream
    /// until 'FIN' or error is received.
    ///
    /// Returns cancellation sender.
    pub fn read(self, span: Span) -> oneshot::Sender<Error<S>> {
        let cancel_sender = self.cancel_sender.clone();

        self.spawn_listen_for_app_error();
        self.spawn_io_loop(span);

        cancel_sender
    }


    /// Spawn a [Self::io_loop] with an additional listener for cancel signals.
    fn spawn_io_loop(mut self, span: Span) {
        let cancel_receiver = self.cancel_receiver.clone();

        let future = async move {
            select! {
                biased;

                e = cancel_receiver.recv() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp("Incoming.cancel_receiver is unavailable".into())
                    ));
                }
                e = self.io_loop() => {
                    self.close(e.map(|_| Error::Finish).unwrap_or_else(|e| e));
                }
            }
        };

        Executor::spawn_void(async move {
            future.instrument(span).await;
        })
    }

    /// Application (the one who receives decoded messages)
    /// might send termination signals to us: [Error::StopSending] or [Error::HangUp].
    ///
    /// Spawn a separate listener to be aware of these signals.
    fn spawn_listen_for_app_error(&self) {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_sender.error_receiver();

        Executor::spawn_void(async move {
            select! {
                biased;

                _ = cancel_receiver.recv() => {
                    // Do nothing.
                },
                app_err = app_error_receiver.recv() => {
                    let app_err = app_err.unwrap_or_else(
                        || Error::HangUp("Incoming.item_sender.error_receiver is unavailable".into())
                    );

                    match &app_err {
                        Error::StopSending(_) |
                        Error::HangUp(_) => {
                            let _ = cancel_sender.send(app_err);
                        }

                        Error::Finish |
                        Error::ResetSending(_) |
                        Error::Decoder(_) |
                        Error::Encoder(_) |
                        Error::Connection(_) => {
                            // Do nothing.
                        }
                    }
                },
            }
        });
    }


    /// Run the primary I/O loop.
    async fn io_loop(&mut self) -> Result<(), Error<S>> {
        loop {
            self.read_from_stream().await?;

            if self.draining {
                let mut current = self.decode_next().await?;

                if current.is_none() {
                    self.send(None, true).await?;
                }

                while let Some(item) = current {
                    let next = self.decode_next().await?;
                    self.send(Some(item), next.is_none()).await?;
                    current = next;
                }

                return Ok(());
            } else {
                while let Some(item) = self.decode_next().await? {
                    self.send(Some(item), false).await?;
                }
            }
        }
    }

    /// Reads bytes from the stream into the [Decoder].
    ///
    /// Does nothing if we've received 'FIN' before.
    async fn read_from_stream(&mut self) -> Result<(), Error<S>> {
        if self.draining {
            return Ok(());
        }

        let buffer = self.decoder.buffer();

        // Empty buffer will lead to an infinite loop,
        // until non-empty buffer is returned.
        // This should not be tolerated.
        assert!(
            buffer.len() > 0,
            "decoder must not provide empty buffers: {}",
            type_name::<S::Decoder>()
        );

        let (buffer, length, fin) = match self
            .backend
            .recv(buffer)
            .await
            .ok_or_else(|| Error::HangUp("Incoming.backend is unavailable".into()))?
        {
            Ok(it) => it,
            Err(e) => {
                return match e {
                    StreamError::Finish => {
                        panic!("bug: received StreamError::Finish, but self.draining is false")
                    }
                    StreamError::StopSending(e) => {
                        if cfg!(debug_assertions) {
                            panic!("bug: received StreamError::StopSending({e}), redundant call")
                        }

                        Err(Error::StopSending(e.into()))
                    }
                    StreamError::ResetSending(e) => Err(Error::ResetSending(e.into())),
                    StreamError::Other(e) => Err(Error::HangUp(e)),
                };
            }
        };

        if length == 0 && !fin {
            return Ok(());
        }

        if fin {
            self.draining = true;
        }

        self.decoder
            .notify_read(buffer, length, fin)
            .await
            .map_err(Error::Decoder)
    }

    /// Tries to decode next item.
    async fn decode_next(&mut self) -> Result<Option<S::Item>, Error<S>> {
        self.decoder.next_item().await.map_err(Error::Decoder)
    }

    /// Sends a decoded message to the application.
    async fn send(&mut self, item: Option<S::Item>, fin: bool) -> Result<(), Error<S>> {
        let empty = item.is_none();
        let payload = match (item, fin) {
            (Some(item), true) => Payload::Last(item),
            (Some(item), false) => Payload::Chunk(item),
            (None, true) => Payload::Done,
            (None, false) => {
                return Ok(());
            }
        };

        match self.item_sender.send_item(payload).await {
            Ok(_) => Ok(()),
            Err(e) => match &e {
                Error::StopSending(_) | Error::HangUp(_) => Err(e),

                Error::Finish => {
                    if empty {
                        return Ok(());
                    }

                    // Panic, because we've lost a message.
                    panic!("bug: attempt to write a new message into a finished stream::Sender");
                }

                Error::ResetSending(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection(_) => {
                    panic!("bug: received unexpected error from stream::Sender: {e}");
                }
            },
        }
    }


    /// Close the stream direction and its resources.
    ///
    /// Notify the application and network peer when necessary.
    fn close(mut self, error: Error<S>) {
        debug!("closing in(read) stream, reason: {}", &error);
        let _ = self.cancel_sender.send(Error::HangUp("".into()));

        match error {
            Error::Finish => {
                // We should have sent 'FIN' to app by now.
                // Nothing to do.
            }

            // We are no more interested in the peer's data,
            // and send 'STOP_SENDING' to him.
            Error::StopSending(e) => {
                self.backend.stop_sending(e.into());
            }

            // Peer sent 'RESET_STREAM' to us, notify the app.
            Error::ResetSending(e) => {
                self.item_sender.terminate(e);
            }

            // Occurred Decoder::Error, notify both sides.
            Error::Decoder(codec_err) => {
                let proto_err = S::on_decoder_error(&codec_err);
                self.item_sender.terminate_due_decoder(codec_err);
                self.backend.stop_sending(proto_err.into());
            }

            // Occurred connection error, notify the app.
            Error::Connection(e) => {
                self.item_sender.terminate_due_connection(e);
            }

            // Internal error, notify both sides.
            Error::HangUp(e) => {
                self.item_sender.hangup(e);
                self.backend.stop_sending(S::on_hangup().into());
            }

            Error::Encoder(codec_err) => {
                panic!(
                    "bug: received unexpected encoder error in incoming stream: {}",
                    &codec_err
                );
            }
        }
    }
}
