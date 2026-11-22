//! The WebSocket session runner shared by every venue.
//!
//! [`run`] keeps one venue connection alive until cancelled. It reconnects with jittered
//! backoff (100 ms doubling to 5 s, reset after 60 s connected), treats a quiet line as dead,
//! sends client pings when the venue wants them, rotates connections before the venue's
//! lifetime limit without a gap event, and caps frame size. Every text frame goes to the tape
//! (never blocking) and, when it parses, downstream as a [`FeedEvent`].

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

use domain::{Symbol, Venue};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::tape::TapeFrame;
use crate::{FeedError, FeedEvent, FeedStats, MarketFeed, SessionPolicy};

/// Default limit for one inbound frame or message.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 4 << 20;
/// Default time a connection must stay up before reconnect backoff starts over.
pub const DEFAULT_BACKOFF_RESET_AFTER: Duration = Duration::from_secs(60);

/// Where and how to connect.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Venue, for events, tape and logs.
    pub venue: Venue,
    /// WebSocket URL (`wss://…`, or `ws://…` for fakes).
    pub url: String,
    /// Symbols to subscribe.
    pub symbols: Vec<Symbol>,
    /// Handshake limit (default 5 s).
    pub connect_timeout: Duration,
    /// Larger frames close the connection (default 4 MiB).
    pub max_frame_bytes: usize,
    /// A connection up this long resets reconnect backoff to its base (default 60 s).
    pub backoff_reset_after: Duration,
}

/// Where a session sends its output.
#[derive(Debug, Clone)]
pub struct Outputs {
    /// Parsed events; awaited, so a slow consumer slows the socket rather than losing deltas.
    pub events: mpsc::Sender<FeedEvent>,
    /// Raw frames for the tape; never awaited, a full queue drops the frame.
    pub tape: Option<mpsc::Sender<TapeFrame>>,
}

/// A connected venue socket.
pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens the WebSocket within `cfg.connect_timeout`, with the frame-size cap applied.
pub async fn connect(cfg: &SessionConfig) -> Result<Ws, FeedError> {
    let ws_cfg = WebSocketConfig::default()
        .max_message_size(Some(cfg.max_frame_bytes))
        .max_frame_size(Some(cfg.max_frame_bytes));
    let attempt = connect_async_with_config(cfg.url.as_str(), Some(ws_cfg), true);
    match timeout(cfg.connect_timeout, attempt).await {
        Err(_) => Err(FeedError::Timeout {
            what: format!("websocket connect {}", cfg.url),
            after: cfg.connect_timeout,
        }),
        Ok(Err(e)) => Err(ws_error(&cfg.url, e)),
        Ok(Ok((ws, _response))) => Ok(ws),
    }
}

fn ws_error(url: &str, e: tungstenite::Error) -> FeedError {
    FeedError::WebSocket {
        url: url.to_owned(),
        source: Box::new(e),
    }
}

/// Keeps the venue connection alive until `cancel` fires or downstream goes away.
pub async fn run<F: MarketFeed>(
    feed: Arc<F>,
    cfg: SessionConfig,
    out: Outputs,
    stats: Arc<FeedStats>,
    cancel: CancellationToken,
) {
    let policy = feed.policy();
    let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(5));
    loop {
        let attempt = tokio::select! {
            () = cancel.cancelled() => return,
            r = connect_and_subscribe(feed.as_ref(), &cfg) => r,
        };
        match attempt {
            Err(e) => warn!(venue = %cfg.venue, error = %e, "connect failed"),
            Ok(ws) => {
                info!(venue = %cfg.venue, url = %cfg.url, "connected");
                let connected_at = Instant::now();
                let session = Session {
                    feed: feed.as_ref(),
                    cfg: &cfg,
                    policy,
                    out: &out,
                    stats: &stats,
                };
                match session.drive(ws, &cancel).await {
                    End::Stop => return,
                    End::Dropped(reason) => {
                        FeedStats::incr(&stats.reconnects);
                        warn!(venue = %cfg.venue, %reason, "connection dropped");
                        let gone = FeedEvent::Disconnected { venue: cfg.venue };
                        if out.events.send(gone).await.is_err() {
                            return;
                        }
                        if connected_at.elapsed() >= cfg.backoff_reset_after {
                            backoff.reset();
                        }
                    }
                }
            }
        }
        let delay = backoff.next_delay();
        tokio::select! {
            () = cancel.cancelled() => return,
            () = sleep(delay) => {}
        }
    }
}

async fn connect_and_subscribe<F: MarketFeed>(
    feed: &F,
    cfg: &SessionConfig,
) -> Result<Ws, FeedError> {
    let mut ws = connect(cfg).await?;
    for frame in feed.subscribe(&cfg.symbols) {
        ws.send(Message::text(frame))
            .await
            .map_err(|e| ws_error(&cfg.url, e))?;
    }
    Ok(ws)
}

enum Rotation {
    /// The new connection is live; `gap` if the old one died before it delivered.
    Done {
        ws: Box<Ws>,
        first: Message,
        gap: bool,
    },
    /// The new connection failed; the old one is still up.
    Failed,
}

enum End {
    /// Cancelled, or downstream is gone: stop for good.
    Stop,
    /// The connection is unusable: reconnect.
    Dropped(String),
}

struct Session<'a, F> {
    feed: &'a F,
    cfg: &'a SessionConfig,
    policy: SessionPolicy,
    out: &'a Outputs,
    stats: &'a FeedStats,
}

impl<F: MarketFeed> Session<'_, F> {
    async fn drive(&self, mut ws: Ws, cancel: &CancellationToken) -> End {
        let mut last_rx = Instant::now();
        let mut pinged = false;
        let mut rotate_at = self.policy.max_age.map(|age| Instant::now() + age);
        loop {
            let idle_deadline = last_rx + self.policy.idle_timeout;
            let ping_deadline = match self.policy.client_ping {
                Some(p) if !pinged => Some(last_rx + p.after_idle),
                _ => None,
            };
            tokio::select! {
                () = cancel.cancelled() => {
                    close(&mut ws).await;
                    return End::Stop;
                }
                () = sleep_until(idle_deadline) => {
                    return End::Dropped(format!("no frame for {:?}", self.policy.idle_timeout));
                }
                () = sleep_until_some(ping_deadline) => {
                    if let Some(p) = self.policy.client_ping
                        && let Err(e) = ws.send(Message::text(p.payload)).await
                    {
                        return End::Dropped(format!("ping failed: {e}"));
                    }
                    pinged = true;
                }
                () = sleep_until_some(rotate_at) => match self.rotate(&mut ws, &mut last_rx, cancel).await {
                    Err(end) => return end,
                    Ok(Rotation::Failed) => rotate_at = Some(Instant::now() + Duration::from_secs(60)),
                    Ok(Rotation::Done { ws: new_ws, first, gap }) => {
                        let mut old = std::mem::replace(&mut ws, *new_ws);
                        close(&mut old).await;
                        if gap {
                            // The old connection died before the new one delivered: updates may
                            // be missing, so downstream must resync as after any drop.
                            FeedStats::incr(&self.stats.reconnects);
                            let gone = FeedEvent::Disconnected { venue: self.cfg.venue };
                            if self.out.events.send(gone).await.is_err() {
                                return End::Stop;
                            }
                        }
                        info!(venue = %self.cfg.venue, gap, "rotated connection");
                        last_rx = Instant::now();
                        pinged = false;
                        rotate_at = self.policy.max_age.map(|age| Instant::now() + age);
                        if let Err(end) = self.handle(first).await {
                            return end;
                        }
                    }
                },
                msg = ws.next() => match msg {
                    None => return End::Dropped("stream ended".into()),
                    Some(Err(e)) => {
                        self.count_read_error(&e);
                        return End::Dropped(e.to_string());
                    }
                    Some(Ok(m)) => {
                        last_rx = Instant::now();
                        pinged = false;
                        if let Err(end) = self.handle(m).await {
                            return end;
                        }
                    }
                },
            }
        }
    }

    /// Opens and subscribes a second connection while still reading the old one, and returns
    /// the new one with its first message once it is live. The old connection keeps delivering
    /// meanwhile (updating `last_rx`), so the handover loses nothing; any overlap is resolved
    /// downstream by sequence numbers.
    async fn rotate(
        &self,
        old: &mut Ws,
        last_rx: &mut Instant,
        cancel: &CancellationToken,
    ) -> Result<Rotation, End> {
        let limit = self.policy.idle_timeout;
        let next = async {
            let mut ws = connect_and_subscribe(self.feed, self.cfg).await?;
            match timeout(limit, ws.next()).await {
                Ok(Some(Ok(first))) => Ok((ws, first)),
                Ok(Some(Err(e))) => Err(ws_error(&self.cfg.url, e)),
                Ok(None) => Err(ws_error(
                    &self.cfg.url,
                    tungstenite::Error::ConnectionClosed,
                )),
                Err(_) => Err(FeedError::Timeout {
                    what: format!("first frame on rotated connection {}", self.cfg.url),
                    after: limit,
                }),
            }
        };
        tokio::pin!(next);
        let mut old_open = true;
        loop {
            tokio::select! {
                // Old socket first: its buffered frames go downstream before the new socket's
                // first frame, and its death is noticed before the handover completes.
                biased;
                () = cancel.cancelled() => return Err(End::Stop),
                m = old.next(), if old_open => match m {
                    Some(Ok(m)) => {
                        *last_rx = Instant::now();
                        match self.handle(m).await {
                            Ok(()) => {}
                            Err(End::Stop) => return Err(End::Stop),
                            Err(End::Dropped(_)) => old_open = false,
                        }
                    }
                    Some(Err(e)) => {
                        self.count_read_error(&e);
                        old_open = false;
                    }
                    None => old_open = false,
                },
                r = &mut next => return match r {
                    Ok((ws, first)) => Ok(Rotation::Done { ws: Box::new(ws), first, gap: !old_open }),
                    Err(e) if old_open => {
                        warn!(venue = %self.cfg.venue, error = %e, "rotation failed, retrying in 60s");
                        Ok(Rotation::Failed)
                    }
                    Err(e) => Err(End::Dropped(format!("connection ended during rotation, which failed: {e}"))),
                },
            }
        }
    }

    fn count_read_error(&self, e: &tungstenite::Error) {
        if matches!(e, tungstenite::Error::Capacity(_)) {
            FeedStats::incr(&self.stats.oversized_frames);
        }
    }

    async fn handle(&self, msg: Message) -> Result<(), End> {
        match msg {
            Message::Text(text) => {
                let received_at = Instant::now();
                FeedStats::incr(&self.stats.frames);
                if let Some(tape) = &self.out.tape {
                    let frame = TapeFrame::now(self.cfg.venue, text.as_str().to_owned());
                    if tape.try_send(frame).is_err() {
                        FeedStats::incr(&self.stats.tape_dropped);
                    }
                }
                match self.feed.parse(text.as_str()) {
                    Ok(Some(update)) => {
                        let event = FeedEvent::Depth {
                            update,
                            received_at,
                        };
                        if self.out.events.send(event).await.is_err() {
                            return Err(End::Stop);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        FeedStats::incr(&self.stats.malformed_frames);
                        debug!(venue = %self.cfg.venue, error = %e, "malformed frame");
                    }
                }
                Ok(())
            }
            Message::Binary(_) => {
                FeedStats::incr(&self.stats.malformed_frames);
                Ok(())
            }
            Message::Close(_) => Err(End::Dropped("closed by venue".into())),
            // Pings are answered by tungstenite on the next read; pongs only prove liveness.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(()),
        }
    }
}

/// Sends a close frame, giving up after a short wait so shutdown stays bounded.
async fn close(ws: &mut Ws) {
    let _ = timeout(Duration::from_millis(500), ws.close(None)).await;
}

async fn sleep_until_some(deadline: Option<Instant>) {
    match deadline {
        Some(d) => sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Exponential backoff with jitter: each delay is in `[d/2, d]` where `d` doubles from `base`
/// up to `cap`.
#[derive(Debug)]
struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
    seed: RandomState,
}

impl Backoff {
    fn new(base: Duration, cap: Duration) -> Self {
        Backoff {
            base,
            cap,
            attempt: 0,
            seed: RandomState::new(),
        }
    }

    fn next_delay(&mut self) -> Duration {
        let full = self
            .base
            .saturating_mul(1u32 << self.attempt.min(20))
            .min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        let mut h = self.seed.build_hasher();
        h.write_u32(self.attempt);
        let fraction = (h.finish() % 1_000) as u32; // 0..1000
        full / 2 + full / 2 * fraction / 1_000
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_4_backoff_doubles_with_jitter_up_to_cap_and_resets() {
        let base = Duration::from_millis(100);
        let cap = Duration::from_secs(5);
        let mut b = Backoff::new(base, cap);
        let mut full = base;
        for _ in 0..10 {
            let d = b.next_delay();
            assert!(
                d >= full / 2 && d <= full,
                "{d:?} not in [{:?}, {full:?}]",
                full / 2
            );
            full = (full * 2).min(cap);
        }
        assert!(b.next_delay() <= cap);
        b.reset();
        assert!(b.next_delay() <= base);
    }
}
