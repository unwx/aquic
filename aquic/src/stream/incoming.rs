use std::any::type_name;
use std::borrow::Cow;

use crate::backend::stream::InStreamBackend;
use crate::debug_panic;
use crate::exec::{Runtime, SendOnMt};
use crate::log;
use crate::stream::{Chunk, Decoder, Error, Payload};
use crate::sync::SmartRc;
use crate::sync::mpmc::oneshot::{self, OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::tracing::StreamSpan;
use crate::{Spec, backend};
use futures::{FutureExt, select_biased};
use tracing::{Instrument, Level, Span};

/// An incoming direction of QUIC stream,
/// acts like a bridge between protocol backend and application listener for a single stream.
///
/// `network_peer <-> quic_backend <-> Incoming<S> <-> local_application`.
///
/// Its role is to read incoming stream data from backend,
/// decode it, and send it to app.
///
/// The entire flow is sequential: implementation won't read from backend,
/// unless the app is ready to receive the data.
pub(crate) struct Incoming<S, CId>
where
    S: Spec,
    CId: SendOnMt + Unpin + 'static,
{
    /// A reference to protocol specification.
    spec: SmartRc<S>,

    /// An API to communicate with QUIC implementation.
    backend: InStreamBackend<CId>,

    /// Decoder for decoding raw bytes stream.
    decoder: S::StreamDecoder,

    /// Buffered chunks of bytes.
    decoder_in_batch: Option<Vec<Chunk>>,

    /// Batch of decoded items.
    decoder_out_batch: Vec<S::StreamItem>,

    /// Channel for sending decoded items.
    item_sender: stream::Sender<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error<S>>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error<S>>,

    /// `true`, if 'FIN' was received from backend.
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
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        let decoder = spec.new_stream_decoder();
        let decoder_in_batch = Some(Vec::with_capacity(4));
        let decoder_out_batch = Vec::with_capacity(1);

        Self {
            spec,
            backend,
            decoder,
            decoder_in_batch,
            decoder_out_batch,
            item_sender,
            cancel_sender,
            cancel_receiver,
            draining: false,
        }
    }

    /// Starts the stream I/O loop in a separate task.
    ///
    /// Listens for data from QUIC backend,
    /// decodes it,
    /// sends it to app.
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
                        || Error::HangUp(format!("'{}.cancel_receiver' is unavailable", type_name::<Self>()).into())
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
                        || Error::HangUp(format!("'{}.item_sender.error_receiver' is unavailable", type_name::<Self>()).into())
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
            self.decode_bytes().await?;
            self.send_to_app().await?;

            if self.draining {
                return Ok(());
            }
        }
    }

    /// Reads bytes from the stream into the [Self::decoder_in_batch].
    ///
    /// Does nothing if we've received 'FIN' before.
    async fn read_from_stream(&mut self) -> Result<(), Error<S>> {
        if self.draining {
            return Ok(());
        }

        let batch = match self.decoder_in_batch.take() {
            Some(it) => it,
            None => {
                return Err(Self::dpanic_or_hangup(
                    format!(
                        "'{}.decoder_in_batch' is absent at 'read_from_stream()' point",
                        type_name::<Self>()
                    )
                    .into(),
                ));
            }
        };

        let (batch, fin) = match self
            .backend
            .recv(batch, self.spec.stream_decoder_max_batch_size())
            .await
            .map_err(|e| {
                Error::HangUp(
                    format!(
                        "'{}.backend' API is unavailable: {}",
                        type_name::<Self>(),
                        e
                    )
                    .into(),
                )
            })? {
            Ok(it) => it,
            Err(e) => {
                return match e {
                    backend::Error::StreamFinish => {
                        self.decoder_in_batch = Some(Vec::new());
                        self.draining = true;
                        return Ok(());
                    }
                    backend::Error::StreamStopSending(e) => {
                        debug_panic!(
                            "'{}.backend' API responded with '{}::StreamStopSending({e})': is this a redundant call?",
                            type_name::<Self>(),
                            type_name::<backend::Error>()
                        );
                        Err(Error::StopSending(e.into()))
                    }
                    backend::Error::StreamResetSending(e) => Err(Error::ResetSending(e.into())),
                    other => Err(Error::HangUp(
                        format!(
                            "'{}.backend' API responded with '{}'",
                            type_name::<Self>(),
                            other
                        )
                        .into(),
                    )),
                };
            }
        };

        if fin {
            self.draining = true;
        }

        self.decoder_in_batch = Some(batch);
        Ok(())
    }

    /// Tries to decode [Self::decoder_in_batch] into [Self::decoder_out_batch].
    async fn decode_bytes(&mut self) -> Result<(), Error<S>> {
        let Some(in_batch) = self.decoder_in_batch.as_mut() else {
            return Err(Self::dpanic_or_hangup(
                format!(
                    "'{}.decoder_in_buffer' is absent at 'decode_bytes()' point",
                    type_name::<Self>()
                )
                .into(),
            ));
        };

        self.decoder
            .decode(in_batch, &mut self.decoder_out_batch, self.draining)
            .await
            .map_err(Error::Decoder)?;

        in_batch.clear();
        Ok(())
    }

    /// Sends all decoded items from [Self::decoder_out_batch] to the application.
    async fn send_to_app(&mut self) -> Result<(), Error<S>> {
        if self.decoder_out_batch.is_empty() && self.draining {
            return self.send_item(None, true).await;
        }

        while let Some(item) = self.decoder_out_batch.pop() {
            self.send_item(
                Some(item),
                self.draining && self.decoder_out_batch.is_empty(),
            )
            .await?;
        }

        Ok(())
    }

    /// Sends a decoded message to the application.
    async fn send_item(&mut self, item: Option<S::StreamItem>, fin: bool) -> Result<(), Error<S>> {
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
                Error::StopSending(_) => Err(e),
                Error::HangUp(e) => Err(Error::HangUp(
                    format!(
                        "'{}.item_sender' is unavailable: {}",
                        type_name::<Self>(),
                        e
                    )
                    .into(),
                )),

                Error::Finish => Err(Self::dpanic_or_hangup(
                    format!(
                        "'{}.item_sender' returned 'Error::Finish' on attempt to send an item, but no 'FIN was received before",
                        type_name::<Self>()
                    )
                    .into(),
                )),

                Error::ResetSending(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => Err(Self::dpanic_or_hangup(
                    format!(
                        "'{}.item_sender' returned unexpected error: {}",
                        type_name::<Self>(),
                        e
                    )
                    .into(),
                )),
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
                let proto_err = S::on_stream_decoder_error(&codec_err);
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
                let e: Cow<'static, str> = format!(
                    "'{}' received unexpected encoder error: {}",
                    type_name::<Self>(),
                    codec_err
                )
                .into();

                debug_panic!("{e}");
                self.item_sender.hangup(e);
                self.backend.stop_sending(S::on_hangup().into());
                span.on_internal();
            }
        };
    }

    fn dpanic_or_hangup(msg: Cow<'static, str>) -> Error<S> {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
