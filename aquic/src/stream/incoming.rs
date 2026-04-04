use crate::backend::StableConnectionId;
use crate::core::InStreamBackend;
use crate::debug_panic;
use crate::exec::Runtime;
use crate::log;
use crate::stream::{Decoder, Error, Payload};
use crate::sync::SmartRc;
use crate::sync::mpmc::oneshot::{self, OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::tracing::StreamSpan;
use crate::{Spec, backend};
use futures::{FutureExt, future, select_biased};
use std::any::type_name;
use std::borrow::Cow;
use tracing::{Instrument, Level, Span};

/// An incoming QUIC stream direction,
/// acts like a bridge between protocol backend and application listener for a single stream.
///
/// `Network Peer >> QUIC Backend >> Incoming<S, CId> >> Application`.
///
/// Its role is to:
/// 1) Receive incoming stream data from `QUIC Backend`.
/// 2) Decode the data.
/// 3) Send decoded items to `Application`.
///
/// The entire flow is sequential: implementation won't read from `QUIC Backend`,
/// unless the `Application` is ready to receive the data.
pub(crate) struct Incoming<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    /// An API to communicate with a QUIC Backend.
    backend: InStreamBackend<S, CId>,

    /// Decoder for decoding raw bytes stream.
    decoder: Option<S::StreamDecoder>,

    /// Channel for sending decoded items.
    item_sender: stream::Sender<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error<S>>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error<S>>,

    /// `true` if `decoder` is empty and expects new raw bytes data.
    decoder_drained: bool,

    /// `true` if 'FIN' is received.
    stream_finished: bool,
}

impl<S, CId> Incoming<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    pub(crate) fn new(
        spec: SmartRc<S>,
        backend: InStreamBackend<S, CId>,
        item_sender: stream::Sender<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        let decoder = spec.new_stream_decoder();

        Self {
            backend,
            decoder: Some(decoder),
            item_sender,
            cancel_sender,
            cancel_receiver,
            decoder_drained: true,
            stream_finished: false,
        }
    }

    /// Starts the stream I/O loop in a separate task.
    ///
    /// - Receives incoming stream data from `QUIC Backend`.
    /// - Decodes the data.
    /// - Sends decoded items to `Application`.
    ///
    /// Returns a cancellation sender.
    ///
    /// **Note**: only few variants of [Error] are allowed to be sent via this sender:
    /// - [Error::Reset],
    /// - [Error::Connection].
    pub(crate) fn read(mut self, span: StreamSpan) -> oneshot::Sender<Error<S>> {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_sender.error_receiver();

        let app_error_future = || async move {
            let app_err = app_error_receiver.recv().await.unwrap_or_else(|| {
                Error::HangUp(
                    format!(
                        "'{}' decoded items '{}' error channel is unavailable",
                        type_name::<Self>(),
                        type_name::<S::StreamItem>()
                    )
                    .into(),
                )
            });

            match &app_err {
                Error::Stop(_) | Error::HangUp(_) => app_err,

                // We produce each of this errors, ignore.
                Error::Finish
                | Error::Reset(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => future::pending().await,
            }
        };

        let future = async move {
            select_biased! {
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::Finish).unwrap_or_else(|e| e));
                }

                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp(format!(
                            "'{}' cancellation-signal channel is unavailable",
                            type_name::<Self>()
                        ).into())
                    ));
                }

                app_err = app_error_future().fuse() => {
                    self.close(app_err);
                },
            }
        };

        Runtime::spawn_void(async move {
            future.instrument(span.into()).await;
        });

        cancel_sender
    }


    async fn io_loop(&mut self) -> Result<(), Error<S>> {
        loop {
            if self.decoder_drained {
                self.read_from_stream().await?;
            }

            if let Some(item) = self.decode().await? {
                self.send_to_app(item).await?;
            }

            if self.stream_finished {
                return Ok(());
            }
        }
    }

    async fn read_from_stream(&mut self) -> Result<(), Error<S>> {
        if self.stream_finished || !self.decoder_drained {
            return Ok(());
        }
        let Some(decoder) = self.decoder.take() else {
            return Ok(());
        };

        self.decoder_drained = false;

        let call_result = self.backend.recv(decoder).await;
        let backend_result = call_result.map_err(|e| {
            Error::HangUp(
                format!(
                    "'{}' QUIC backend RPC channel is unavailable: {}",
                    type_name::<Self>(),
                    e
                )
                .into(),
            )
        })?;

        match backend_result {
            Ok(decoder) => {
                self.decoder = Some(decoder);
                Ok(())
            }
            Err(e) => match e {
                backend::Error::StreamReset(e) => Err(Error::Reset(e.into())),
                backend::Error::Closed | backend::Error::UnknownConnection => {
                    Err(Error::Connection)
                }
                other => Err(Error::HangUp(
                    format!(
                        "'{}' QUIC backend RPC channel responded with '{}'",
                        type_name::<Self>(),
                        other
                    )
                    .into(),
                )),
            },
        }
    }

    async fn decode(&mut self) -> Result<Option<Payload<S::StreamItem>>, Error<S>> {
        if self.stream_finished || self.decoder_drained {
            return Ok(None);
        }
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(None);
        };

        let item = decoder.decode().map_err(Error::Decoder)?;
        match &item {
            Some(it) => {
                if it.is_fin() {
                    self.stream_finished = true;
                }
            }
            None => {
                self.decoder_drained = true;
            }
        }

        Ok(item)
    }

    async fn send_to_app(&mut self, item: Payload<S::StreamItem>) -> Result<(), Error<S>> {
        match self.item_sender.send_item(item).await {
            Ok(_) => Ok(()),
            Err(e) => match &e {
                Error::Stop(_) => Err(e),
                Error::HangUp(e) => Err(Error::HangUp(
                    format!(
                        "'{}' decoded items '{}' channel is unavailable: {}",
                        type_name::<Self>(),
                        type_name::<S::StreamItem>(),
                        e
                    )
                    .into(),
                )),

                Error::Finish
                | Error::Reset(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => Err(Self::dpanic_or_hangup(
                    format!(
                        "'{}' decoded items '{}' channel is unavailable",
                        type_name::<Self>(),
                        type_name::<S::StreamItem>()
                    )
                    .into(),
                )),
            },
        }
    }


    fn close(mut self, error: Error<S>) {
        if self.cancel_sender.send(error.clone()).is_err() {
            return;
        };

        let log_level = match &error {
            Error::Finish => Level::DEBUG,
            Error::Stop(_) => Level::DEBUG,
            Error::Reset(_) => Level::DEBUG,
            Error::Decoder(_) => Level::WARN,
            Error::Encoder(_) => Level::ERROR,
            Error::Connection => Level::DEBUG,
            Error::HangUp(_) => Level::WARN,
        };

        log!(log_level, "closing in(read) stream, reason: {}", &error);
        let span = StreamSpan::from(Span::current());

        match error {
            Error::Finish => {
                span.on_fin();
            }

            // We are no more interested in the peer's data,
            // and send 'STOP_SENDING' to him.
            Error::Stop(e) => {
                self.backend.terminate(e);
                span.on_stop_sending(e.into());
            }

            // Peer sent 'RESET_STREAM' to us, notify the app.
            Error::Reset(e) => {
                self.item_sender.terminate(e);
                span.on_reset_stream(e.into());
            }

            // Occurred Decoder::Error, notify both sides.
            Error::Decoder(codec_err) => {
                self.item_sender.terminate_due_decoder(codec_err.clone());
                self.backend.terminate_due_decoder(codec_err);
                span.on_internal();
            }

            // Occurred connection error, notify the app.
            Error::Connection => {
                self.item_sender.terminate_due_connection();
                span.on_connection_close();
            }

            // Internal error, notify both sides.
            Error::HangUp(e) => {
                self.item_sender.hangup(e.clone());
                self.backend.hangup(e);
                span.on_internal();
            }

            Error::Encoder(codec_err) => {
                let e: Cow<'static, str> = format!(
                    "'{}' received an unexpected encoder error: {}",
                    type_name::<Self>(),
                    codec_err
                )
                .into();

                debug_panic!("{e}");
                self.item_sender.hangup(e.clone());
                self.backend.hangup(e);
                span.on_internal();
            }
        };
    }

    fn dpanic_or_hangup(msg: Cow<'static, str>) -> Error<S> {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
