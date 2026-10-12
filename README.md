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

## Running on regtest

A guardian backed by Floresta needs three daemons on regtest. `bitcoind`
mines blocks and funds transactions. Floresta validates through utreexo
proofs, which vanilla bitcoind cannot serve, so
[`utreexod`](https://github.com/utreexo/utreexod) sits between them as a
bridge: it syncs from bitcoind, computes the proofs, and serves them to
`florestad`. The guardian then talks JSON-RPC to florestad only.

Prerequisites: `bitcoind` and `bitcoin-cli`, a `utreexod` binary, and a
`florestad` built from [Floresta](https://github.com/getfloresta/Floresta)
master (`cargo build --release -p florestad`).

Start bitcoind and mine a chain first; utreexod that connects to a
peer at height zero can fail to start syncing, so it joins after
blocks exist:

```sh
# terminal 1
mkdir -p ~/.regtest-stack/{bitcoind,utreexod,florestad}
bitcoind -regtest -datadir="$HOME/.regtest-stack/bitcoind" -fallbackfee=0.0001

# fund a wallet and build a chain (any shell)
bitcoin-cli -regtest -datadir="$HOME/.regtest-stack/bitcoind" createwallet miner
bitcoin-cli -regtest -datadir="$HOME/.regtest-stack/bitcoind" -generate 150

# terminal 2
utreexod --regtest --datadir="$HOME/.regtest-stack/utreexod" \
  --connect=127.0.0.1:18444 --listen=127.0.0.1:18446 \
  --utreexoproofindex --prune=0

# terminal 3
florestad --network regtest --data-dir "$HOME/.regtest-stack/florestad" \
  --connect 127.0.0.1:18446
```

The stack is healthy when `bitcoin-cli ... getblockcount` and
`floresta-cli -n regtest getblockcount` agree. If florestad stays at
zero, restart utreexod (and then florestad) so they re-handshake
against the grown chain.

With the stack up, build and start the guardian:

```sh
cargo build -p fedimintd-floresta

mkdir -p /tmp/guardian-test
FM_FLORESTA_URL=http://127.0.0.1:18442 \
  ./target/debug/fedimintd-floresta --data-dir /tmp/guardian-test
```

`FM_FLORESTA_URL` (or `--floresta-url`) is required; the guardian
refuses to start without it. A working boot logs the metrics server,
the API on port 8174, and `Setup UI running at http://127.0.0.1:8175`;
opening that address shows fedimint's Guardian Setup page, and the
process stays up while blocks are mined. Forming a federation from
there (config generation, multiple guardians, peg-ins) is the
end-to-end demo and has its own setup flow.

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
