---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "**/Cargo.lock"
  - "**/rust-toolchain.toml"
---

# Rust rules

These come on top of the shared rules in `.claude/SDLC.md`. The checker is `.claude/checks/rust.sh`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` (both `--locked`), and `cargo deny check` on full runs when `deny.toml` exists.

- **Toolchain**: pinned in `rust-toolchain.toml` (with rustfmt and clippy), edition 2024. `Cargo.lock` is committed. Shared versions live in the root `[workspace.dependencies]`.
- **Workspace lints**, set once in the root `Cargo.toml` and inherited with `[lints] workspace = true`:
  - `[workspace.lints.rust]` `unsafe_code = "forbid"`, `missing_docs = "warn"`
  - `[workspace.lints.clippy]` `unwrap_used = "deny"`, `expect_used = "deny"`
  - `clippy.toml`: `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`
- **Money**: `rust_decimal::Decimal` behind newtypes (`Price`, `Qty`, `Notional`). Parse exchange strings straight into `Decimal`. Never `f32`/`f64` for prices, quantities, fees or P&L.
- **Errors**: `thiserror` enums in library crates; `anyhow` only in `apps/*` wiring. Add context once per layer. No `unwrap`/`expect`/`panic!` outside tests (the lint enforces it). Indexing and slicing must not panic on untrusted data. Match on variants, not on messages.
- **Async**: Tokio. Every spawned task is owned (a `JoinSet` or a `JoinHandle` an owner awaits) and exits on a `tokio_util::sync::CancellationToken`. No blocking calls in async code (use `spawn_blocking`). Never hold a `std::sync::Mutex`/`RwLock` guard across `.await`; prefer one task owning the state (actor) over locks.
- **Channels**: bounded `mpsc`. Hot paths use `try_send` and count drops. Latest-value data uses `tokio::sync::watch`. Dropping the sender closes the channel.
- **I/O**: every network call has a timeout (`reqwest::Client` with `timeout` + `connect_timeout`; WebSocket idle/read timeouts). Cap untrusted message sizes (tungstenite `max_message_size`/`max_frame_size`). TLS via rustls.
- **SQL**: `sqlx` with bind parameters only. Transactions are committed explicitly; drop means rollback.
- **State**: no `static mut`, no global singletons with interior mutability. Config is passed explicitly.
- **Logging**: `tracing` with structured fields. Secrets are wrapped in `secrecy::SecretString` and never logged.
- **Visibility**: `pub(crate)` by default; `pub` only for what other crates use. Every `pub` item has a doc comment.
- **Dependencies**: a new crate needs an ADR line. Use `default-features = false` when it trims the tree. `Cargo.lock` is changed by cargo only.

## Tests

- Unit tests sit in `#[cfg(test)] mod tests`. Behavior tests go through the public API in the crate's `tests/` dir.
- Name tests after the AC: `#[test] fn ac_2_rejects_duplicate_order()`. Table cases loop over an array of named cases and put the case name in the assert message.
- Async tests use `#[tokio::test(start_paused = true)]` with `tokio::time::advance`. Never sleep to synchronize.
- Write fakes by hand against the trait; no mock frameworks. Fake WebSocket/HTTP peers run on a local `TcpListener`.
- Parsers of untrusted input get a `proptest` property test in the normal suite, plus a `cargo-fuzz` target under `fuzz/` (nightly, run on demand).
- Run one test: `cargo test -p <crate> ac_2`.

## Review blockers

- `unwrap`/`expect`/`panic!`, or panicking index/slice on untrusted data, in non-test code.
- `f32`/`f64` for money; `unsafe`.
- A blocking call inside async code; a lock guard held across `.await`.
- A spawned task with no owner or no cancellation path; an unbounded channel on a hot path.
- A network call without a timeout; an uncapped message size from an untrusted peer.

## Self-check

- [ ] `cargo fmt` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `Cargo.lock` was updated by cargo, not by hand
- [ ] New `pub` items have doc comments and are needed outside the crate
