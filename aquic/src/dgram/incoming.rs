use crate::AsyncRuntime;
use crate::backend::StableConnectionId;
use crate::core::InDgramBackend;
use crate::{
    Spec,
    dgram::Error,
    sync::{
        SmartRc, dgram,
        mpmc::oneshot::{self, OneshotReceiver, OneshotSender},
    },
};
use crate::{backend, log};
use crate::{debug_panic, dgram::Decoder, tracing::DgramSpan};
use futures::{FutureExt, future, select_biased};
use std::any::type_name;
use std::borrow::Cow;
use tracing::{Instrument, Level, Span, trace};

/// An incoming direction of a QUIC datagram flow,
/// acts like a bridge between protocol backend and application listener for a single connection.
///
/// `Network Peer >> QUIC Backend >> Incoming<S, CId> >> Application`.
///
/// Its role is to:
/// 1) Receive incoming datagrams from `QUIC Backend`.
/// 2) Decode the data.
/// 3) Send decoded items to `Application`.
///
/// If `Application` is not ready to process decoded items,
/// they are going to be dropped.
pub(crate) struct Incoming<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    /// An API to communicate with a QUIC Backend.
    backend: InDgramBackend<S, CId>,

    /// Decoder for decoding datagrams.
    decoder: Option<S::DgramDecoder>,

    /// Channel for sending decoded items.
    item_sender: dgram::Sender<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error>,

    /// `true` if `decoder` is empty and expects new datagrams.
    decoder_drained: bool,
}


impl<S, CId> Incoming<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    pub fn new(
        spec: SmartRc<S>,
        backend: InDgramBackend<S, CId>,
        item_sender: dgram::Sender<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        let decoder = spec.new_datagram_decoder();

        Self {
            backend,
            decoder: Some(decoder),
            item_sender,
            cancel_sender,
            cancel_receiver,
            decoder_drained: false,
        }
    }

    /// Starts the datagram I/O loop in a separate task.
    ///
    /// - Receives incoming datagrams from `QUIC Backend`.
    /// - Decodes the datagrams.
    /// - Sends decoded items to `Application`.
    ///
    /// Returns a cancellation sender.
    ///
    /// **Note**: only a [Error::Connection] is allowed to be sent via this sender.
    pub fn read<AR: AsyncRuntime>(mut self, span: DgramSpan) -> oneshot::Sender<Error> {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_sender.error_receiver();

        let app_error_future = || async move {
            let app_err = app_error_receiver.recv().await.unwrap_or_else(|| {
                Error::HangUp(
                    format!(
                        "'{}' decoded items '{}' error channel is unavailable",
                        type_name::<Self>(),
                        type_name::<S::DgramItem>()
                    )
                    .into(),
                )
            });

            match &app_err {
                Error::End | Error::HangUp(_) => app_err,

                // We produce this error, ignore.
                Error::Connection => future::pending().await,
            }
        };

        let future = async move {
            select_biased! {
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::End).unwrap_or_else(|e| e));
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

        AR::spawn_void(async move {
            future.instrument(span.into()).await;
        });

        cancel_sender
    }


    async fn io_loop(&mut self) -> Result<(), Error> {
        loop {
            self.recv_dgrams().await?;

            if let Some(item) = self.decode_dgram() {
                self.send_item(item)?;
            }
        }
    }

    async fn recv_dgrams(&mut self) -> Result<(), Error> {
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

    fn decode_dgram(&mut self) -> Option<S::DgramItem> {
        self.decoder.as_mut().and_then(|it| it.decode())
    }

    fn send_item(&mut self, item: S::DgramItem) -> Result<(), Error> {
        match self.item_sender.try_send(item) {
            Ok(true) => Ok(()),
            Ok(false) => {
                trace!(
                    "unable to send a decoded datagram into sync::dgram channel: channel is full"
                );
                Ok(())
            }
            Err(e) => match &e {
                Error::End => Err(e),
                Error::HangUp(e) => Err(Error::HangUp(
                    format!(
                        "'{}' decoded items '{}' channel is unavailable: {}",
                        type_name::<Self>(),
                        type_name::<S::DgramItem>(),
                        e
                    )
                    .into(),
                )),
                other => Err(Self::dpanic_or_hangup(
                    format!(
                        "'{}' decoded items '{}' channel returned an unexpected error: {}",
                        type_name::<Self>(),
                        type_name::<S::DgramItem>(),
                        other
                    )
                    .into(),
                )),
            },
        }
    }


    fn close(&mut self, error: Error) {
        if self.cancel_sender.send(error.clone()).is_err() {
            return;
        };

        let log_level = match &error {
            Error::End | Error::Connection => Level::DEBUG,
            Error::HangUp(_) => Level::WARN,
        };

        log!(
            log_level,
            "closing in(read) datagram direction, reason: {}",
            &error
        );

        let span = DgramSpan::from(Span::current());
        match error {
            Error::End => span.on_normal_end(),
            Error::Connection => {
                span.on_connection_close();
                self.item_sender.terminate_due_connection();
            }
            Error::HangUp(e) => {
                span.on_internal();
                self.item_sender.hangup(e);
            }
        }
    }

    fn dpanic_or_hangup(msg: Cow<'static, str>) -> Error {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
