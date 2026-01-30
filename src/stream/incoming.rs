use crate::Spec;
use crate::backend::stream::{InStreamBackend, StreamError};
use crate::exec::{Runtime, SendOnMt};
use crate::log;
use crate::stream::{Decoder, Error, Payload};
use crate::sync::oneshot::{OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::sync::{SmartRc, oneshot};
use crate::tracing::StreamSpan;
use futures::{FutureExt, select_biased};
use tracing::{Instrument, Level, Span};

/// An incoming direction of QUIC stream,
/// acts like a bridge between protocol backend and application listener.
///
/// Its role is to read incoming stream data,
/// decode it, and send it via channel.
///
/// The entire flow is sequential: implementation won't read from the stream,
/// unless the listener is ready to receive the data.
pub(crate) struct Incoming<S, CId>
where
    S: Spec,
    CId: SendOnMt + Unpin + 'static,
{
    /// An API to communicate with QUIC implementation.
    backend: InStreamBackend<CId>,

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

impl<S, CId> Incoming<S, CId>
where
    S: Spec,
    CId: Clone + SendOnMt + Unpin + 'static,
{
    pub fn new(
        spec: SmartRc<S>,
        backend: InStreamBackend<CId>,
        item_sender: stream::Sender<S>,
    ) -> Self {
        let decoder = spec.new_decoder();
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
    /// Returns a cancellation sender.
    ///
    /// **Note**: only few variants of [Error] are allowed to be sent via this sender:
    /// - [Error::ResetSending],
    /// - [Error::Connection].
    pub fn read(self, span: StreamSpan) -> oneshot::Sender<Error<S>> {
        let cancel_sender = self.cancel_sender.clone();

        self.spawn_listen_for_app_error();
        self.spawn_io_loop(span);

        cancel_sender
    }


    /// Spawn a [Self::io_loop] with an additional listener for cancel signals.
    fn spawn_io_loop(mut self, span: StreamSpan) {
        let cancel_receiver = self.cancel_receiver.clone();

        let future = async move {
            select_biased! {
                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp("Incoming.cancel_receiver is unavailable".into())
                    ));
                }
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::Finish).unwrap_or_else(|e| e));
                }
            }
        };

        Runtime::spawn_void(async move {
            future.instrument(span.into()).await;
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

        Runtime::spawn_void(async move {
            select_biased! {
                _ = cancel_receiver.recv().fuse() => {
                    // Do nothing.
                },
                app_err = app_error_receiver.recv().fuse() => {
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
                        Error::Connection => {
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
                    let fin = self.decoder.is_fin();
                    self.send(Some(item), fin).await?;
                    current = if fin { None } else { self.decode_next().await? };
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

        let (chunks, fin) = match self
            .backend
            .recv(self.decoder.max_batch_size())
            .await
            .map_err(|e| Error::HangUp(format!("Incoming.backend is unavailable: {e}").into()))?
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

        if chunks.is_empty() && !fin {
            return Ok(());
        }

        if fin {
            self.draining = true;
        }

        self.decoder.read(chunks, fin).await.map_err(Error::Decoder)
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
            (Some(item), false) => Payload::Item(item),
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
                | Error::Connection => {
                    panic!("bug: received unexpected error from stream::Sender: {e}");
                }
            },
        }
    }


    /// Close the stream direction and its resources.
    ///
    /// Notify the application and network peer when necessary.
    fn close(mut self, error: Error<S>) {
        if self.cancel_sender.send(Error::HangUp("".into())).is_err() {
            return;
        };

        let log_level = match &error {
            Error::Finish => Level::DEBUG,
            Error::StopSending(_) => Level::DEBUG,
            Error::ResetSending(_) => Level::DEBUG,
            Error::Decoder(_) => Level::WARN,
            Error::Encoder(_) => Level::ERROR,
            Error::Connection => Level::DEBUG,
            Error::HangUp(_) => Level::WARN,
        };

        log!(log_level, "closing in(read) stream, reason: {}", &error);
        let span = StreamSpan::from(Span::current());

        match error.clone() {
            Error::Finish => {
                span.on_fin();
            }

            // We are no more interested in the peer's data,
            // and send 'STOP_SENDING' to him.
            Error::StopSending(e) => {
                self.backend.stop_sending(e.into());
                span.on_stop_sending(e.into());
            }

            // Peer sent 'RESET_STREAM' to us, notify the app.
            Error::ResetSending(e) => {
                self.item_sender.terminate(e);
                span.on_reset_stream(e.into());
            }

            // Occurred Decoder::Error, notify both sides.
            Error::Decoder(codec_err) => {
                let proto_err = S::on_decoder_error(&codec_err);
                self.item_sender.terminate_due_decoder(codec_err);
                self.backend.stop_sending(proto_err.into());
                span.on_internal();
            }

            // Occurred connection error, notify the app.
            Error::Connection => {
                self.item_sender.terminate_due_connection();
                span.on_connection_close();
            }

            // Internal error, notify both sides.
            Error::HangUp(e) => {
                self.item_sender.hangup(e);
                self.backend.stop_sending(S::on_hangup().into());
                span.on_internal();
            }

            Error::Encoder(codec_err) => {
                span.on_internal();
                panic!(
                    "bug: received unexpected encoder error in incoming stream: {}",
                    &codec_err
                );
            }
        };
    }
}
