# ADR 0001: Rust workspace, venue adapters and dependency baseline

Status: Accepted
Spec: docs/specs/0002-market-data-connectivity.md

## Context
Spec 0002 creates the first Rust code: a workspace, four crates and the `engine record` binary. These need to support:
- untrusted WebSocket input from two venues with different heartbeat, sequencing and connection-lifetime rules
- exact decimal parsing of prices and quantities
- a replayable tape of raw frames, which spec 0003 (books) and spec 0005 (replay) build on

The repo has no dependencies yet; `.claude/rules/rust.md` sets the conventions.

## Options
1. **Smallest change: our own adapters on tokio-tungstenite + reqwest**, with one shared session runner and a small per-venue protocol.
   - Cost: we write and maintain about 1.5k lines and track venue API changes ourselves.
   - Risk: we get each venue's edge cases wrong. The fake-server tests in spec 0002 cover the known ones.
2. **The `ccxt` crate (4.5.93, MIT)**, reviewed from its crate metadata. It is the transpiled CCXT engine, with 106 features and every venue. Its `watchOrderBook` keeps the book internally.
   - It hides the raw frames (needed for the tape) and the venue sequence fields (needed for our own Binance `U`/`u` and OKX `seqId`/checksum validation in spec 0003).
   - It brings in a large dependency tree we would not audit.
   - It stays useful as a reference for message shapes.

## Decision
Option 1. The spec needs raw frames and exact sequence semantics, and those are exactly what a unified API abstracts away.

## Shape

**Crates added:**
- `crates/domain`: `Venue`, `Symbol`, and the `Price`/`Qty` newtypes over `Decimal`.
- `crates/exchange-core`:
  - `DepthUpdate`, `Level`, `FeedEvent`, `FeedError`, `FeedStats`
  - `session::run`: connect, subscribe, read loop, size cap, heartbeat, reconnect with backoff, proactive rotation
  - `tape::{Writer, read}`
  - `MarketFeed` (the protocol trait)
- `crates/exchange-binance` and `crates/exchange-okx`: the protocol implementation, frame parser and `fetch_instruments` (REST) for each venue.
- `apps/engine`: config (TOML), wiring, the `record` subcommand. CLI parsing uses `std::env::args`.
- `fuzz/`: a cargo-fuzz crate kept outside the workspace (nightly, run on demand).

**The trait.** Declared in `exchange-core`, which consumes it. It has two implementations today: Binance and OKX.
```rust
pub trait MarketFeed: Send + Sync + 'static {
    fn subscribe(&self, symbols: &[Symbol]) -> Vec<String>;               // frames to send after connect
    fn parse(&self, frame: &str) -> Result<Option<DepthUpdate>, ParseError>; // None = ack/pong/other
    fn policy(&self) -> SessionPolicy;                                     // idle timeout, client ping, max age
}
```
`SessionPolicy` is plain data:
- `idle_timeout`: Binance 60 s
- `client_ping`: OKX sends `"ping"` after 25 s idle and waits 5 s
- `max_age`: Binance 23 h

All venue-specific behavior is data or parsing, so the runner has no `if venue == …` branches.

**Concurrency:**
- `engine` owns a root `CancellationToken` and a `JoinSet`. Per venue it spawns:
  - one session task: one connection carrying all three symbols
  - one tape writer task, which owns that venue's files
- Feed events go session → consumer on a bounded `mpsc` (4096) with `send().await`. Book deltas must not be lost; a slow consumer pushes back to TCP.
- Raw frames go session → tape on a bounded `mpsc` (8192) with `try_send`; a full queue drops the frame and increments `tape_dropped`.
- On cancel:
  - Sessions send a WebSocket Close and return.
  - Tape writers drain their queue and finish the zstd frame.
  - `main` waits up to 2 s for the `JoinSet`, then aborts whatever remains.
- **Rotation (AC-6):** the session opens and subscribes a second socket and waits for its first data frame. Only then does it switch reads over and close the old socket.

**Errors:**
- `FeedError` is a `thiserror` enum with variants `RegionBlocked { venue, status }`, `UnknownSymbol { venue, symbol }`, `Timeout`, `Http`, `WebSocket` and `Config`.
- Startup errors (metadata, config) are returned to `main`.
- Errors after the connection is up are handled inside the session (reconnect), never returned.
- `ParseError` is counted, never propagated.

**Time:**
- `tokio::time::Instant` gives the local receive time, so tests can pause and advance the clock.
- `SystemTime` gives `recv_ns` for the tape.
- Tape file names (UTC hour) are computed with a 15-line days-to-civil-date conversion, not a date crate.

**Data:** no database. Tapes follow the spec 0002 AC-9 format. The tape line format is a compatibility surface: readers must accept unknown extra fields.

**New dependencies.** All are mainstream and actively maintained, with permissive licenses. `deny.toml` allows MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0 and CDLA-Permissive-2.0 (webpki-roots), which is exactly what the locked tree needs.

| Crate | Version | License | Why std or existing deps won't do |
|---|---|---|---|
| tokio (rt-multi-thread, net, time, signal, sync, macros) | 1.54 | MIT | Async runtime; std has no async I/O |
| tokio-util | 0.7 | MIT | `CancellationToken` |
| tokio-tungstenite (rustls) + futures-util | 0.30 / 0.3 | MIT / MIT-Apache | WebSocket client and test server; std has none |
| reqwest (rustls, json, no default features) | 0.13 | MIT/Apache | HTTPS REST with timeouts |
| serde, serde_json | 1.0 | MIT/Apache | Venue JSON, tape lines |
| rust_decimal | 1.43 | MIT | Exact money (rules: no floats) |
| thiserror (libraries), anyhow (`apps/engine` only) | 2.0 / 1.0 | MIT/Apache | Typed errors / binary wiring |
| tracing, tracing-subscriber | 0.1 / 0.3 | MIT | Structured logging (rules) |
| zstd | 0.14 | BSD-3-Clause | Tape compression; std has none |
| toml | 1.2 | MIT/Apache | Config file |
| proptest (dev) | 1.12 | MIT/Apache | Property tests for parsers (rules) |
| libfuzzer-sys (`fuzz/` only, outside the workspace) | 0.4 | MIT/Apache/NCSA | cargo-fuzz harness for the parser targets (rules) |

Each crate's exact feature set is pinned in the root `Cargo.toml`. `Cargo.lock` is committed.

**Amendment (2026-11-22, implementation).** rustls uses the `aws-lc-rs` crypto provider, not `ring`. reqwest 0.13's `rustls` feature selects aws-lc-rs and the platform certificate verifier. Switching to ring would mean `rustls-no-provider` plus a process-global `install_default()` call, which goes against the no-global-state rule. tokio-tungstenite shares the same provider, so the tree carries one TLS stack. Building it needs cmake, which is installed here and on GitHub runners. aws-lc-rs/aws-lc-sys are licensed under ISC and Apache-2.0 terms, which `deny.toml` already allows.

**Observability:** `FeedStats` holds atomics per venue (`frames`, `malformed_frames`, `oversized_frames`, `reconnects`, `tape_dropped`), logged every 60 s with `tracing`. Prometheus arrives in M6.

## Consequences and rollback
- **We own API drift.** Endpoints and channel names live in config, and AC-12, the live smoke test, detects breakage.
- **Rotation overlap.** During rotation (AC-6) the new connection's first Binance update may overlap the old stream's last one. Spec 0003's sync must drop updates with `u ≤ last` and resync on a gap; it has to do that after reconnects anyway.
- **Rollback.** There is no persisted state apart from tapes, so rollback is a revert.
