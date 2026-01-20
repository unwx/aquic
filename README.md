# aquic

## What is `aquic`

`aquic` is a QUIC library that provides the instruments to easily build and run your own protocol over QUIC. It avoids
locking you into a specific async runtime, QUIC protocol implementation, or socket implementation.

It sits in between these layers, handling I/O loop routines for you, while aiming to be as thin and fast as possible.

## Safety

There is currently no `unsafe` usage in the code, though this might change upon release.

In any case, `unsafe` usage will be either completely banned or strictly restricted.

## State

No releases have been made yet; the library is currently under development.

The goal is to support the following QUIC implementations:

- [quiche](https://github.com/cloudflare/quiche)
- [quinn-proto](https://github.com/quinn-rs/quinn)

And the following async runtimes:

- [tokio](https://github.com/tokio-rs/tokio)
- [monoio](https://github.com/bytedance/monoio)
