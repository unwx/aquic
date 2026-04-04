# aquic

## What is `aquic`

`aquic`, Adapter QUIC (or Agnostic QUIC), is a QUIC protocol library.

Using `aquic`, you can easily build and use your own protocol over QUIC 
without locking yourself into a QUIC implementation provider, an asynchronous runtime, or a platform.

You can use [tokio](https://github.com/tokio-rs/tokio) + [quiche](https://github.com/cloudflare/quiche) today,
and switch to [smol](https://github.com/smol-rs/smol) + [quinn-proto](https://github.com/quinn-rs/quinn) tomorrow.

A migration between `Send` <-> `Send` async runtimes is possible and seamless.

A migration between `Send` <-> `!Send` is possible,
but due to its nature may require code changes to make it thread-safe (`!Send` -> `Send`), 
or more performant (`Send` -> `!Send`). 

## State

No releases have been made yet; the library is currently under development.

The TODO list before the first release:
- [x] Add QUIC streams support: codecs, async stream processors (`stream` mod).
- [x] Add [tokio](https://github.com/tokio-rs/tokio) async runtime support.
- [x] Add [monoio](https://github.com/bytedance/monoio) async runtime support.
- [x] Add [quinn-proto](https://github.com/quinn-rs/quinn) QUIC implementation support.
- [x] Add [quiche](https://github.com/cloudflare/quiche) QUIC implementation support.
- [x] Add QUIC [datagrams](https://datatracker.ietf.org/doc/html/rfc9221) support: codecs, async datagrams processors (`dgram` mod).
- [ ] Add 0-RTT support.
- [ ] Add [smol](https://github.com/smol-rs/smol) async runtime support; 
      allow users to implement `spawn()`, `sleep()` methods
      for `Send` and `!Send` runtimes.
- [ ] Add Linux/Windows optimized `net::Socket` implementation, with `std` as a fallback.
- [ ] Add `QuicBackend::migrate_scids()` method;
      This method updates each connection's source ID with a new `ConnectionIdGenerator`, retiring the old ones.
      The goal is to allow active server migrations (if they are behind a Load-Balancer),
      using something like [CRIU](https://github.com/checkpoint-restore/criu).
- [ ] Add `QuicBackend::connection_export()` & `QuicBackend::connection_import()` methods;
      These methods detach a connection from `T: QuicBackend`, allowing to import it into other instance of `T`.
      The goal is to allow actively balance connections between CPU cores, when using thread-per-core async runtimes.
- [ ] Add `core` mod implementation, that unites everything with a network I/O loop.
- [ ] Add primary entry point for this library, to create client/server endpoints.
- [ ] Add unit tests.
- [ ] Add high-level tests, imitating a real-world deployed scenario.
- [ ] Add micro-benchmarks.
- [ ] Add high-level benchmarks, imitating a real-world deployed scenario.

The TODO list after the first release:
- [ ] Add [io_uring](https://man7.org/linux/man-pages/man7/io_uring.7.html) `net::Socket` implementation, when the `monoio` feature is enabled.
- [ ] Add active load balancing for thread-per-core async runtimes, using `QuicBackend::connection_export()` & `QuicBackend::connection_import()`.
- [ ] Add [tquic](https://github.com/Tencent/tquic) QUIC implementation support.
- [ ] Add [ngtcp2](https://github.com/ngtcp2/ngtcp2) QUIC implementation support.
- [ ] Expose `Rust <- C` bindings.
- [ ] Expose `C <- Java` bindings.
- [ ] Add [AF_XDP](https://docs.ebpf.io/linux/concepts/af_xdp/) `net::Socket` implementation.
