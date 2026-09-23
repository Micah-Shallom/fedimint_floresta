# fedimint-floresta

> **Status: early work in progress 🏗️.** The adapter is under active
> development and not ready for use. Follow the
> [issues](https://github.com/Micah-Shallom/fedimint_floresta/issues) for
> the roadmap.

A [Floresta](https://github.com/getfloresta/Floresta)-powered Bitcoin
backend for [Fedimint](https://github.com/fedimint/fedimint) guardians.

## Synopsis

Today a Fedimint guardian must run (or trust) a full `bitcoind` or an
Esplora server, the heaviest dependency in the guardian stack. This
project replaces it with Floresta, a pruned, Utreexo-based lightweight
node, letting a guardian run a complete self-validating Bitcoin setup on
commodity hardware.

This works because Fedimint's guardian backend interface
(`IServerBitcoinRpc`) is block-scan based: the wallet module detects
peg-ins by fetching blocks sequentially and scanning them locally, never
by random-access UTXO or address queries. That access pattern is exactly
what a Utreexo node can serve.

## Architecture

Two crates:

- `crates/fedimint-floresta`: `FlorestaClient`, an implementation of
  Fedimint's `IServerBitcoinRpc` trait over `florestad`'s JSON-RPC.
- `crates/fedimintd-floresta`: a thin guardian binary that wires
  `FlorestaClient` into the stock Fedimint server, so no fork of
  Fedimint is required.

Known limitation by design: Floresta has no mempool, so `get_feerate`
returns no estimate for now; on regtest Fedimint uses a fixed feerate
and does not need one. Fee estimation for other networks is tracked as
future work.

## Developing

This project uses [`just`](https://github.com/casey/just). Run `just`
to list recipes; `just pre-push` runs the same checks as CI (fmt,
clippy, build, test).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
