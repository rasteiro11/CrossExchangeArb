# 0002: Market-data connectivity (Binance + OKX)

Status: Approved
Ticket: none (roadmap M1, `docs/roadmap.md`, weeks 1–2)

## Problem
The order-book engine (M2) and the strategy (M3) need a continuous, normalized and replayable stream of depth updates from both venues. None exists yet: there is no Rust workspace, no exchange connector and no recorded data to test against.

## Non-goals
- Order-book reconstruction, snapshot sync, sequence or checksum validation (spec 0003). This spec delivers raw depth deltas plus the sequence fields that 0003 needs.
- Private streams, API keys, order placement (0009).
- Prometheus, Postgres, ClickHouse, gRPC, and any Go or Python code. Counters are in-process and logged.
- Measuring RTT from candidate hosting regions. That is a manual ops task in week 1, outside the code.
- Venues and markets other than Binance/OKX × BTC/ETH/SOL-USDT.

## Acceptance criteria
"Fake server" means a WebSocket/HTTP server on a local `TcpListener`, written in the tests. Live tests are `#[ignore]` and run with `cargo test -- --ignored`.

- **AC-1 Normalized depth.** Given recorded Binance `depthUpdate` frames and OKX `books` frames, when they are parsed, then each becomes a `DepthUpdate` with these fields:
  - `venue` and `symbol`: `BTCUSDT` and `BTC-USDT` both become `Symbol { base: BTC, quote: USDT }`
  - bid/ask levels as `Decimal` parsed exactly from the strings, with zero quantity meaning "remove level"
  - the venue sequence fields (Binance `U`/`u`; OKX `seqId`/`prevSeqId`/`checksum`, plus a snapshot flag)
  - the exchange timestamp, and the local receive time from a monotonic clock
- **AC-2 Instrument metadata.** Given a fake REST server serving recorded `exchangeInfo` / `instruments` responses, when the feed starts, then every configured symbol has tick size, lot step, min quantity and min notional. A configured symbol the venue doesn't list fails startup with an error that names the symbol.
- **AC-3 Region block.** Given a venue that answers HTTP 451 or 403, when the feed starts, then startup fails within one request with `venue <name> unreachable from this region (HTTP 451)` and does not retry.
- **AC-4 Reconnect.** Given an established session, when the fake server drops the TCP connection, then:
  - downstream receives `FeedEvent::Disconnected { venue }` before any later update
  - the session reconnects and resubscribes all symbols within 5 s
  - reconnect backoff starts at 100 ms, doubles with jitter up to 5 s, and resets after 60 s connected
  - the `reconnects` counter increments
- **AC-5 Dead connection.** With time paused:
  - **OKX:** no inbound frame for 25 s makes the client send `ping`. No reply within 5 s more counts as a drop, handled as in AC-4.
  - **Binance:** server pings are answered with a pong carrying the same payload. 60 s with no inbound frame counts as a drop.
- **AC-6 Binance 24 h limit.** Given a Binance session connected for 23 h, it reconnects proactively. The new connection is subscribed before the old one is closed, so no `Disconnected` is emitted.
- **AC-7 Malformed input.** Given frames that are invalid JSON, have missing fields, carry non-numeric prices or name unknown channels, when received, then each one increments `malformed_frames{venue}` and is skipped, and the session stays connected. A proptest feeds arbitrary bytes to both parsers and they never panic. `fuzz/` has a cargo-fuzz target per parser.
- **AC-8 Size cap.** A frame larger than 4 MiB closes the connection, increments `oversized_frames{venue}` and triggers the AC-4 reconnect.
- **AC-9 Tape.** Every inbound text frame is written to a zstd NDJSON tape as one line: `{"recv_ns":…,"venue":"binance","frame":"<raw>"}`.
  - Files rotate hourly, or at 256 MiB, under `<tape_dir>/<venue>/YYYY-MM-DDTHH.ndjson.zst`.
  - Replaying a tape through the parser yields the same `DepthUpdate` sequence as the live run that recorded it.
  - The recorder never blocks the feed: if its bounded queue is full, frames are dropped and `tape_dropped` increments.
- **AC-10 Shutdown.** On SIGINT or cancellation, every task exits within 2 s, and the open tape file is finished so that `zstd -t` passes.
- **AC-11 Timeouts.** REST calls use a 3 s connect and 10 s total timeout. WebSocket connect has a 5 s timeout. A fake server that never answers produces a timeout error within those bounds.
- **AC-12 Live smoke (ignored).** Against the real venues, within 10 s of start, `engine record` emits at least one `DepthUpdate` for each of the 6 (venue, symbol) pairs. That proves both venues are reachable from this machine.

## Interfaces touched
- **New Cargo workspace:** root `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, and a `.gitignore` covering `target/` and `tapes/`.
  - Crates: `crates/domain`, `crates/exchange-core`, `crates/exchange-binance`, `crates/exchange-okx`.
  - Binary: `apps/engine`.
- **CLI:** `engine record --config configs/engine.dev.toml`.
- **Config (`configs/engine.dev.toml`):** symbols, per-venue endpoint URLs (overridable for fakes), `tape_dir`, timeouts.
- **Tape file format** as in AC-9. It becomes the replay input for 0003 and 0005, so later changes must stay readable.
- **Exported Rust API:** `domain::{Symbol, Venue, Price, Qty, Instrument}`, `exchange_core::{DepthUpdate, FeedEvent, MarketFeed}`.

## Risks
- **Data loss or corruption:** none. The data is public and read-only. A tape truncated by a crash loses at most the open zstd frame; that is acceptable.
- **Exposure:**
  - No credentials are involved; only public endpoints are used.
  - Tapes contain no PII.
  - Exchange frames are untrusted input, hence AC-7 and AC-8.
- **Breaking callers:** none exist yet. The tape format becomes a compatibility surface from here on.
- **Rollout / rollback:** local binary only; rollback is a revert.
- **Venue limits:** rapid reconnects could hit Binance's connection-rate limit (about 300 connections per 5 min per IP), so the backoff in AC-4 caps the rate at 1/s after a few failures.

## Assumptions and open questions
- Market-data endpoints and channels are confirmed against the official docs in week 1; config makes them easy to change:
  - **Binance:** combined stream `/stream?streams=<sym>@depth@100ms/…`
  - **OKX:** `wss://ws.okx.com/ws/v5/public` (port 443; 8443 is being retired), channel `books`
- One connection per venue carries all three symbols.
- The connectors are our own. CCXT is consulted only as a reference; ADR-0001 records the half-day spike result and the dependency list.
- Counters are atomics in `exchange-core` and logged every 60 s with `tracing`. Prometheus arrives in M6.
- Tapes go to `tapes/` (gitignored). Retention is manual for now.

## Implementation notes (2026-11-22)
The ACs are unchanged. How the tests prove them:
- **AC-5 / AC-6 "time paused":** tokio's paused clock auto-advances whenever the runtime is idle, and with real TCP sockets in the fake servers that fires idle timeouts while I/O is still in flight. So the session tests run on real time with short durations passed in through `SessionPolicy` (300–600 ms). The real venue values (25 s ping + 5 s, 60 s idle, 23 h rotation) are asserted in each venue crate's policy test.
- **AC-1 receive time** is carried by `FeedEvent::Depth { update, received_at }` rather than a field on `DepthUpdate`. That keeps `DepthUpdate` comparable for the replay check in AC-9.
- **AC-6 handover:** while rotating, the old connection keeps delivering until the new one's first frame arrives, so nothing is lost. Overlapping updates are left to spec 0003's sequence checks.
- **Live findings:** OKX sends `checksum: 0` on every `books` frame, and port 8443 is being retired, so the default is now 443. Both are recorded in `docs/roadmap.md` for spec 0003.
