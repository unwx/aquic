use std::any::type_name;
use std::borrow::Cow;

use crate::log;
use crate::{debug_panic, dgram::Decoder, sync::TryRecvError, tracing::DgramSpan};
use bytes::Bytes;
use futures::{FutureExt, select_biased};
use tracing::{Instrument, Level, Span, trace};

use crate::{
    Spec,
    dgram::Error,
    exec::Runtime,
    sync::{
        SmartRc, dgram,
        mpmc::oneshot::{self, OneshotReceiver, OneshotSender},
        mpsc::unbounded::{self, UnboundedReceiver},
    },
};

/// An incoming direction of QUIC datagram flow,
/// acts like a bridge between protocol backend and application listener for a single connection.
///
/// `network_peer <-> quic_backend <-> Incoming<S> <-> local_application`.
///
/// Its role is to receive datagrams from backend,
/// decode them, and send them to app.
pub(crate) struct Incoming<S: Spec> {
    /// A reference to protocol specification.
    spec: SmartRc<S>,

    /// Datagrams receiver.
    dgram_receiver: unbounded::Receiver<Bytes>,

    /// Decoder for decoding datagrams.
    decoder: S::DgramDecoder,

    /// Batch of datagrams.
    decoder_in_batch: Vec<Bytes>,

    /// Batch of decoded items.
    decoder_out_batch: Vec<S::DgramItem>,

    /// Channel for sending decoded items.
    item_sender: dgram::Sender<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error>,
}


impl<S: Spec> Incoming<S> {
    pub fn new(
        spec: SmartRc<S>,
        dgram_receiver: unbounded::Receiver<Bytes>,
        item_sender: dgram::Sender<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        let decoder = spec.new_dgram_decoder();
        let decoder_in_batch = Vec::with_capacity(4);
        let decoder_out_batch = Vec::with_capacity(1);

        Self {
            spec,
            dgram_receiver,
            decoder,
            decoder_in_batch,
            decoder_out_batch,
            item_sender,
            cancel_sender,
            cancel_receiver,
        }
    }

    /// Starts the datagram I/O loop in a separate task.
    ///
    /// Listens for datagrams from QUIC backend,
    /// decodes them,
    /// sends them to app.
    ///
    /// Returns a cancellation sender.
    ///
    /// **Note**: only a [Error::Connection] is allowed to be sent via this sender.
    pub fn read(self, span: DgramSpan) -> oneshot::Sender<Error> {
        let cancel_sender = self.cancel_sender.clone();

        self.spawn_listen_for_app_error();
        self.spawn_io_loop(span);

        cancel_sender
    }


    /// Spawn a [Self::io_loop] with an additional listener for cancel signals.
    fn spawn_io_loop(mut self, span: DgramSpan) {
        let cancel_receiver = self.cancel_receiver.clone();

        let future = async move {
            select_biased! {
                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp(format!("'{}.cancel_receiver' is unavailable", type_name::<Self>()).into())
                    ));
                }
                r = self.io_loop().fuse() => {
                    if let Err(e) = r {
                        self.close(e);
                    }
                }
            }
        };

        Runtime::spawn_void(async move {
            future.instrument(span.into()).await;
        })
    }

    /// Application (the one who receives decoded messages)
    /// might drop the receiver.
    ///
    /// Spawn a separate listener to be aware when it is no more interested in datagrams.
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
                        Error::End |
                        Error::HangUp(_) => {
                            let _ = cancel_sender.send(app_err);
                        }
                        Error::Connection => {
                            // Do nothing.
                        }
                    }
                },
            }
        });
    }


    /// Run the primary I/O loop.
    async fn io_loop(&mut self) -> Result<(), Error> {
        loop {
            self.recv_dgrams().await?;
            self.decode_dgrams()?;
            self.send_items()?;
        }
    }

    /// Reads at least one datagram from [Self::dgram_receiver]
    /// into [Self::decoder_in_batch].
    async fn recv_dgrams(&mut self) -> Result<(), Error> {
        if !self.decoder_in_batch.is_empty() {
            debug_panic!(
                "'{}.decoder_in_batch' should be empty at 'recv_dgrams()' point",
                type_name::<Self>()
            );
            self.decoder_in_batch.clear();
        }

        let mut total_bytes = 0;

        {
            let Some(dgram) = self.dgram_receiver.recv().await else {
                return Err(Error::HangUp(
                    format!("'{}.dgram_receiver' is unavailable", type_name::<Self>()).into(),
                ));
            };

            total_bytes += dgram.len();
            self.decoder_in_batch.push(dgram);
        }

        while total_bytes < self.spec.dgram_decoder_max_batch_size() {
            let dgram = match self.dgram_receiver.try_recv() {
                Ok(it) => it,
                Err(TryRecvError::Empty) => {
                    break;
                }
                Err(TryRecvError::Closed) => {
                    return Err(Error::HangUp(
                        format!("'{}.dgram_receiver' is unavailable", type_name::<Self>()).into(),
                    ));
                }
            };

            total_bytes += dgram.len();
            self.decoder_in_batch.push(dgram);
        }

        Ok(())
    }

    /// Decodes [Self::decoder_in_batch] into [Self::decoder_out_batch].
    fn decode_dgrams(&mut self) -> Result<(), Error> {
        if self.decoder_in_batch.is_empty() {
            return Ok(());
        }

        self.decoder
            .decode(&mut self.decoder_in_batch, &mut self.decoder_out_batch);

        self.decoder_in_batch.clear();
        Ok(())
    }

    /// Sends all decoded items from [Self::decoder_out_batch] to the application.
    fn send_items(&mut self) -> Result<(), Error> {
        if self.decoder_out_batch.is_empty() {
            return Ok(());
        }

        while let Some(item) = self.decoder_out_batch.pop() {
            match self.item_sender.try_send(item) {
                Ok(true) => {
                    continue;
                }
                Ok(false) => {
                    trace!("unable to send decoded item into sync::dgram channel: channel is full");
                }
                Err(e) => {
                    return match &e {
                        Error::End => Err(e),
                        Error::HangUp(e) => Err(Error::HangUp(
                            format!(
                                "'{}.item_sender' is unavailable: {}",
                                type_name::<Self>(),
                                e
                            )
                            .into(),
                        )),
                        Error::Connection => Err(Self::dpanic_or_hangup(
                            format!(
                                "'{}.item_sender' returned unexpected error: {}",
                                type_name::<Self>(),
                                e
                            )
                            .into(),
                        )),
                    };
                }
            }
        }

        Ok(())
    }


    fn close(&mut self, error: Error) {
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
