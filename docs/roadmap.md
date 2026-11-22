# Roadmap: Cross-Exchange Crypto Arbitrage Engine v1

Status: Active · Horizon: 12 weeks · Track every milestone through `.claude/SDLC.md`.

## Goal

Build an inventory-based, two-venue spot arbitrage system. It decides whether an opportunity can be captured profitably after fees, depth, precision and risk limits, then executes it, manages one-leg risk, and reconciles P&L. It is not a price-difference detector.

## Decisions (v1)

| Topic | Decision |
|---|---|
| Languages | Rust trading core (one process, Tokio channels), Go control plane (gRPC to the core), Python research, React/TS dashboard |
| Harness | Language-agnostic SDLC harness (spec 0001) before any product code |
| Venues / markets | Binance + OKX; BTC/USDT, ETH/USDT, SOL/USDT |
| Execution | In-process paper simulator plus Binance Spot Testnet and OKX demo trading. Signals always come from mainnet public data. No real money in v1 |
| Orders | Marketable IOC limit orders with strict price protection. No maker strategies |
| Storage | PostgreSQL (trading state, engine is the sole writer); ClickHouse (analytics, M5); raw WebSocket tapes as zstd NDJSON files (replay + fixtures) |
| Deploy | Docker Compose |

**Non-goals for v1:** cross-venue transfers and rebalancing (v1 tracks imbalance and halts at the limit), maker strategies, more venues (Kraken/Bybit), Kubernetes, live capital.

## Architecture

```
            mainnet public WS/REST              testnet/demo signed REST+WS
                     │                                   │
┌────────────────────▼───────────────────────────────────▼─────────────┐
│ engine (Rust, one process)                                           │
│ feed tasks ──watch<BookSnapshot>──▶ strategy/symbol ──mpsc──▶        │
│ execution actor (owns inventory, risk state, ops; journals before    │
│ send) ◀── gateway events (paper | testnet) ◀── control cmds (tonic)  │
│ recorder: raw tape files + analytics events (bounded, drop+count)    │
└──────┬───────────────────────────────┬───────────────────────────────┘
       │ sole writer of trading tables │ gRPC (proto/engine/v1)
   PostgreSQL ◀──── reads ──── control-api (Go, stdlib net/http) ── REST+SSE ──▶ dashboard (React/TS)
   ClickHouse (M5: opportunities, rejections, latency)  ·  tapes ──▶ replay mode ──▶ research (Python)
```

Rules that keep this to two deployables:
- **Exchange connectivity lives only in Rust.** Go never talks to exchanges. It reads Postgres and sends commands to the engine over gRPC.
- **The engine is the only writer of trading state.** Config overrides and kill switches go through gRPC, and the engine persists them.
- **Python never reimplements strategy logic.** Backtesting means running the engine's `replay` subcommand over recorded tapes. Python analyzes the output.
- **The Go control API uses stdlib routing** (method+path patterns), following the "stdlib before deps" rule.

### Repo layout

```
Cargo.toml (workspace: lints, shared deps)   rust-toolchain.toml (pinned, rustfmt+clippy)
crates/domain          Price/Qty/Notional (rust_decimal newtypes), Symbol, Venue, Instrument(tick, step, min_notional), Side, ids
crates/exchange-core   traits MarketFeed / OrderGateway / BalanceSource; WS session (reconnect, backoff+jitter, heartbeat); signing
crates/exchange-binance, exchange-okx   venue rules isolated here
crates/exchange-paper  simulator implementing OrderGateway (fills against live/replayed book, latency, partials, fault injection, seeded)
crates/orderbook       book, apply/validate, walk-the-book VWAP, health/freshness
crates/strategy        fee model, sizing, opportunity scanner, rejection reasons
crates/risk            limits, kill switches, breakers (pure, sync)
crates/execution       state machine, inventory ledger + reservations, journal (sqlx), recovery
apps/engine            Rust bin: wiring, config, tonic server, recorder, `replay` subcommand
apps/control-api       Go module
apps/dashboard         React + TS (Vite)
proto/engine/v1/       engine.proto (+ buf); split by area only when it grows
research/              Python (uv): replay-output analysis, notebooks
migrations/            sqlx migrations (engine owns schema)
deployments/compose.yaml   configs/   docs/{roadmap.md, specs/, adr/}
```

A crate splits further only when it outgrows one responsibility.

### Engine concurrency

- A root `CancellationToken` and a `JoinSet` own every task. If a critical task exits, global trading is disabled before shutdown.
- One feed task per venue connection holds that venue's books and publishes `Arc<BookSnapshot>` (top N levels, seq, local receive `Instant`, healthy flag) on a `watch` per (venue, symbol).
- One strategy task per symbol `select!`s on both venues' `watch.changed()`. It sends `Opportunity` or `Rejection{reason}` down a bounded channel with `try_send`, so stale signals are dropped instead of queued.
- A single execution actor owns inventory, reservations, risk state and open operations, so none of that needs a lock. The risk gate is a synchronous function called inside it.
- Freshness uses the local receive clock. Exchange timestamps are kept for analysis, along with a clock-offset metric.

### Opportunity

The strategy emits a structured opportunity and never executes directly. Fields:

- id, `symbol: Symbol`, buy and sell venue
- buy/sell VWAP, plus `buy_limit_price` / `sell_limit_price`: the worst level walked, used as the IOC price protection
- quantity, gross profit, estimated fees (with `fee_asset` per leg), estimated slippage, expected net profit
- both books' timestamps, and `expires_at` from local monotonic time

It is re-validated against the current books immediately before submission.

### Execution state machine (ADR in M4)

```
Detected → RiskApproved → Reserved → IntentsJournaled → LegsSubmitted (both legs concurrently, IOC limit)
  → BothFilled → Settled
  → Imbalanced → Recovering (cancel remainder | hedge residual with protected IOC | flatten) → Settled | Halted(kill switch)
Leg: PendingNew → Acked → PartiallyFilled → Filled | Canceled | Rejected;  Timeout → Unknown → query by client id → resolve
```

- Client order IDs are alphanumeric only and at most 32 characters (OKX `clOrdId` is the stricter limit). They encode operation, leg and attempt.
- The intent row is committed before the network call. A timeout is never treated as a reject.
- Fills are idempotent on a unique (venue, trade_id). On restart, the engine loads non-terminal operations, queries their status by client ID, then resumes.

### Data

Postgres tables (sqlx migrations, owned by the engine): `operations`, `orders`, `order_events` (append-only), `fills`, `balance_snapshots`, `risk_events`, `config_overrides`. Raw WebSocket frames go to rotating zstd NDJSON tapes from M1 onward. These tapes are both the replay source and the test fixtures.

### Risk defaults (`configs/risk.toml`, enforced only by the risk crate)

| Parameter | Initial |
|---|---|
| Max trade notional | $100 |
| Max open arbitrage operations | 1 |
| Max per-venue exposure | 50% |
| Max unhedged exposure | $25 |
| Daily loss limit (simulated) | $50 |
| Max book age | 500 ms |
| Max execution slippage | 5 bps |

Also:
- global and per-venue kill switches
- maximum daily order count
- maximum inventory imbalance
- breakers for stale data, reject rate and reconciliation mismatch

## Milestones

Each row is one or more SDLC specs. The final ACs are set in each spec.

| Weeks | Milestone | Specs / ADRs | Exit criterion |
|---|---|---|---|
| 0 | M0 Harness | **0001** language-agnostic SDLC harness | Harness ACs green on Go/Rust/Python fixtures; CI green |
| 1–2 | M1 Connectivity | **ADR-0001** architecture + dependency baseline, including a half-day CCXT Rust spike (the default is our own adapters). **0002** market-data connectivity: public WS for both venues, instrument metadata, symbol normalization, reconnect/heartbeat, tape recorder, parser fuzz/proptest | All 6 (venue, symbol) streams normalized; reconnect ≤5 s after a forced drop; malformed frames are counted and skipped, never crash; RTT measured from 2–3 candidate hosting regions |
| 3–4 | M2 Books | **0003** order-book sync: Binance snapshot + diff sequencing; OKX snapshot + incremental with `seqId` and CRC32 checksum; VWAP; staleness | Gap or checksum failure ⇒ book unhealthy within one message and resynced; replaying a 24h tape gives 0 undetected desyncs |
| 5–6 | M3 Strategy | **0004** opportunity engine: depth-aware sizing, per-account fee tier + fee asset, tick/step/min-notional rounding, inventory cap, threshold, expiry, rejection reasons. **0005** replay subcommand + `research/` bootstrap | Golden test: 0.30 BTC → buy VWAP 100,016.67, sell 100,266.67, gross 75.00, fees 30.005 + 30.080, extra 5.00, net 9.915. The same tape replayed twice gives byte-identical output |
| 7–8 | M4 Execution + Risk | **ADR-0002** state machine + journal. **0006** risk engine + kill switches/breakers. **0007** paper execution + recovery + restart recovery. **0008** engine gRPC + minimal Go control API (status, kill switch, config overrides; bearer-token auth, localhost bind) | Success, partial fill, reject, timeout-then-filled and one-leg failure each end Settled or Halted with exposure ≤ $25; a kill -9 mid-operation recovers on restart |
| 9–10 | M5 Venues + Portfolio | **0009** testnet gateways (Binance Spot Testnet, OKX demo). **0010** reconciliation + portfolio/P&L + ClickHouse analytics sink. **0011** dashboard v1: Overview, Opportunities (with rejection reasons), Execution | P&L rebuilt from fills matches the ledger; a reconciliation diff above tolerance trips the breaker |
| 11–12 | M6 Hardening | **0012** ops: config + secrets, OpenTelemetry, Prometheus + Grafana + alerts, full Compose deploy, API docs. **0013** dashboard Portfolio + Analytics | ≥72h continuous paper/testnet soak with forced restarts; state reconciled after each restart |

**Load warning:** weeks 9–10 are the heaviest. Dashboard v1 has three screens, infra and latency charts live in Grafana, and Portfolio + Analytics screens move to M6.

## Success metrics

Do not optimize for the number of detected opportunities. Track:
- gross vs net expected spread
- realized P&L
- fill ratio and partial-fill rate
- hedge cost and opportunity decay
- inventory utilization

The headline metric is `ROI_capital = realized net P&L / average deployed capital`. It includes all fees, execution losses and rebalancing costs.

## Exchange facts to verify in week 1

These are from memory, so the official docs win.

- **Binance:**
  - Books: `<sym>@depth@100ms` plus `GET /api/v3/depth`. Drop events with `u ≤ lastUpdateId`; the first event must span `lastUpdateId+1`; each next `U` must equal the previous `u+1`.
  - Metadata: `exchangeInfo` filters PRICE_FILTER, LOT_SIZE, NOTIONAL.
  - Fees: buy fees are charged in the base asset unless paid in BNB, so BTC inventory drifts. The per-account rate comes from `/api/v3/account/commission`.
  - User-data stream: the listenKey flow is being replaced by WS-API subscribe; confirm which one applies.
  - Testnet: `testnet.binance.vision`.
- **OKX:**
  - Books: `books` channel (snapshot + `update`, `seqId`/`prevSeqId`), with a CRC32 checksum over the top 25 interleaved levels.
  - Metadata: `GET /api/v5/public/instruments` (`tickSz`, `lotSz`, `minSz`). Fees: `GET /api/v5/account/trade-fee`.
  - `clOrdId` is alphanumeric, at most 32 characters.
  - Demo trading uses `wspap` hosts and the `x-simulated-trading: 1` header.
- **Testnet books are synthetic.** Use them only to check order semantics, never for prices or signals.

## Environment notes

- Go 1.22 is installed locally, but the test rules need ≥1.24 (`t.Context()`). `go.mod` pins the current stable release, and `GOTOOLCHAIN=auto` fetches it.
- Postgres and ClickHouse run in Compose (no local `psql`).
- The CI review job references the placeholder `github.com/yourorg/sdlc-harness`. Replace it with the real module path.
