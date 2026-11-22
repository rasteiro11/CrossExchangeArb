//! The trading engine binary.
//!
//! `engine record --config <file>` (spec 0002): loads instrument metadata from both venues,
//! streams depth updates for every configured symbol, records every raw frame to the tape and
//! logs the first update per (venue, symbol) plus counters every 60 s. Ctrl-C stops it within 2 s.

mod config;

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use config::Settings;
use domain::Venue;
use exchange_binance::BinanceFeed;
use exchange_core::http::HttpClient;
use exchange_core::session::{self, Outputs, SessionConfig};
use exchange_core::tape;
use exchange_core::{FeedEvent, FeedStats, MarketFeed};
use exchange_okx::OkxFeed;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, interval, timeout_at};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: engine record --config <file>";
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .init();
    match setup().and_then(|(settings, writers)| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("start runtime")?
            .block_on(record(settings, writers))
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Blocking setup done before the runtime starts: arguments, config file, tape directories.
fn setup() -> anyhow::Result<(Settings, Vec<(Venue, tape::Writer)>)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.as_slice() {
        [cmd, flag, path] if cmd == "record" && flag == "--config" => path,
        _ => bail!(USAGE),
    };
    let settings = config::load(Path::new(config_path))?;
    let writers = VENUES
        .iter()
        .map(|&venue| {
            tape::Writer::new(&settings.tape_dir, venue, tape::DEFAULT_MAX_FILE_BYTES)
                .map(|w| (venue, w))
                .with_context(|| format!("open tape dir {}", settings.tape_dir.display()))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok((settings, writers))
}

const VENUES: [Venue; 2] = [Venue::Binance, Venue::Okx];

async fn record(settings: Settings, writers: Vec<(Venue, tape::Writer)>) -> anyhow::Result<()> {
    let symbols = settings.symbols.clone();
    let http = HttpClient::new(settings.http)?;
    let (binance, okx) = tokio::try_join!(
        exchange_binance::fetch_instruments(&http, &settings.binance.rest_url, &symbols),
        exchange_okx::fetch_instruments(&http, &settings.okx.rest_url, &symbols),
    )
    .context("load instrument metadata")?;
    for i in binance.iter().chain(&okx) {
        info!(
            venue = %i.venue, symbol = %i.symbol, tick = %i.tick_size.0, step = %i.lot_step.0,
            min_qty = %i.min_qty.0, min_notional = %i.min_notional, "instrument"
        );
    }

    let cancel = CancellationToken::new();
    let (events_tx, events_rx) = mpsc::channel(4096);
    let mut tasks = JoinSet::new();
    let mut recorders = JoinSet::new();
    let mut all_stats = Vec::new();
    for (venue, writer) in writers {
        let stats = Arc::new(FeedStats::default());
        all_stats.push((venue, Arc::clone(&stats)));
        let (tape_tx, tape_rx) = mpsc::channel(8192);
        recorders.spawn_blocking(move || (venue, tape::run_blocking(writer, tape_rx)));
        let endpoints = match venue {
            Venue::Binance => &settings.binance,
            Venue::Okx => &settings.okx,
        };
        let cfg = SessionConfig {
            venue,
            url: endpoints.ws_url.clone(),
            symbols: symbols.clone(),
            connect_timeout: settings.ws_connect,
            max_frame_bytes: session::DEFAULT_MAX_FRAME_BYTES,
            backoff_reset_after: session::DEFAULT_BACKOFF_RESET_AFTER,
        };
        let out = Outputs {
            events: events_tx.clone(),
            tape: Some(tape_tx),
        };
        match venue {
            Venue::Binance => spawn_session(
                &mut tasks,
                BinanceFeed::new(&symbols),
                cfg,
                out,
                stats,
                &cancel,
            ),
            Venue::Okx => {
                spawn_session(&mut tasks, OkxFeed::new(&symbols), cfg, out, stats, &cancel)
            }
        }
    }
    drop(events_tx);
    tasks.spawn(consume(events_rx, all_stats, cancel.clone()));

    let mut failed = false;
    tokio::select! {
        r = shutdown_signal() => {
            r?;
            info!("shutting down");
        }
        Some(res) = tasks.join_next() => {
            error!(?res, "a feed task exited early, shutting down");
            failed = true;
        }
        Some(res) = recorders.join_next() => {
            log_recorder(res);
            error!("a tape recorder stopped, shutting down");
            failed = true;
        }
    }
    cancel.cancel();

    // Sessions stop first; that drops the tape senders, which lets the recorders finish.
    let deadline = Instant::now() + SHUTDOWN_LIMIT;
    if timeout_at(deadline, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("tasks still running after {SHUTDOWN_LIMIT:?}, aborting them");
        tasks.abort_all();
    }
    let drained = timeout_at(deadline, async {
        while let Some(res) = recorders.join_next().await {
            log_recorder(res);
        }
    });
    if drained.await.is_err() {
        error!("tape recorders did not finish within {SHUTDOWN_LIMIT:?}");
        failed = true;
    }
    if failed {
        bail!("stopped on a task failure");
    }
    Ok(())
}

/// SIGINT (Ctrl-C) or SIGTERM (`docker stop`, systemd).
async fn shutdown_signal() -> anyhow::Result<()> {
    let mut term = signal(SignalKind::terminate()).context("listen for SIGTERM")?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r.context("listen for SIGINT"),
        _ = term.recv() => Ok(()),
    }
}

fn log_recorder(res: Result<(Venue, std::io::Result<()>), tokio::task::JoinError>) {
    match res {
        Ok((_, Ok(()))) => {}
        Ok((venue, Err(e))) => error!(%venue, error = %e, "tape recorder failed"),
        Err(e) => error!(error = %e, "tape recorder panicked"),
    }
}

fn spawn_session<F: MarketFeed>(
    tasks: &mut JoinSet<()>,
    feed: F,
    cfg: SessionConfig,
    out: Outputs,
    stats: Arc<FeedStats>,
    cancel: &CancellationToken,
) {
    tasks.spawn(session::run(
        Arc::new(feed),
        cfg,
        out,
        stats,
        cancel.clone(),
    ));
}

/// Logs the first update per (venue, symbol), every disconnect, and counters every 60 s.
async fn consume(
    mut events: mpsc::Receiver<FeedEvent>,
    stats: Vec<(Venue, Arc<FeedStats>)>,
    cancel: CancellationToken,
) {
    let mut seen = HashSet::new();
    let mut report = interval(Duration::from_secs(60));
    report.tick().await;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = report.tick() => {
                for (venue, s) in &stats {
                    info!(
                        %venue,
                        frames = FeedStats::get(&s.frames),
                        malformed = FeedStats::get(&s.malformed_frames),
                        oversized = FeedStats::get(&s.oversized_frames),
                        reconnects = FeedStats::get(&s.reconnects),
                        tape_dropped = FeedStats::get(&s.tape_dropped),
                        "feed stats"
                    );
                }
            }
            event = events.recv() => match event {
                None => return,
                Some(FeedEvent::Depth { update, .. }) => {
                    if seen.insert((update.venue, update.symbol.clone())) {
                        info!(venue = %update.venue, symbol = %update.symbol, "first depth update");
                    }
                }
                Some(FeedEvent::Disconnected { venue }) => warn!(%venue, "disconnected, books are stale"),
            },
        }
    }
}
