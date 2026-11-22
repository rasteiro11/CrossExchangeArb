//! Session behavior against a fake WebSocket server (spec 0002 AC-1, AC-4..AC-11).
//!
//! Durations are shortened through `SessionPolicy` so tests run on real sockets in well under a
//! second each; the venue crates assert their real policy values.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use domain::{Symbol, Venue};
use exchange_core::session::{self, Outputs, SessionConfig};
use exchange_core::tape::{self, TapeFrame};
use exchange_core::testkit::{FakeWs, ServerWs, silent_listener};
use exchange_core::{
    ClientPing, DepthUpdate, FeedError, FeedEvent, FeedStats, MarketFeed, ParseError, Sequence,
    SessionPolicy,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(5);

/// Frames `u:<n>` are updates with sequence n; `ack` and `pong` carry nothing; anything else is
/// malformed.
struct TestFeed {
    policy: SessionPolicy,
}

impl MarketFeed for TestFeed {
    fn subscribe(&self, symbols: &[Symbol]) -> Vec<String> {
        let names: Vec<String> = symbols.iter().map(ToString::to_string).collect();
        vec![format!("sub:{}", names.join(","))]
    }

    fn parse(&self, frame: &str) -> Result<Option<DepthUpdate>, ParseError> {
        if frame == "ack" || frame == "pong" {
            return Ok(None);
        }
        let n: u64 = frame
            .strip_prefix("u:")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| ParseError(format!("bad frame {frame:?}")))?;
        Ok(Some(update(n)))
    }

    fn policy(&self) -> SessionPolicy {
        self.policy
    }
}

fn btc() -> Symbol {
    Symbol::new("BTC", "USDT")
}

fn update(n: u64) -> DepthUpdate {
    DepthUpdate {
        venue: Venue::Binance,
        symbol: btc(),
        bids: vec![],
        asks: vec![],
        sequence: Sequence::Binance {
            first_update_id: n,
            last_update_id: n,
        },
        snapshot: false,
        exchange_ts_ms: n,
    }
}

fn quiet_policy() -> SessionPolicy {
    SessionPolicy {
        idle_timeout: WAIT,
        client_ping: None,
        max_age: None,
    }
}

struct Harness {
    server: FakeWs,
    events: mpsc::Receiver<FeedEvent>,
    stats: Arc<FeedStats>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Harness {
    async fn start(policy: SessionPolicy, tape: Option<mpsc::Sender<TapeFrame>>) -> Self {
        Self::start_with(policy, tape, |_| {}).await
    }

    async fn start_with(
        policy: SessionPolicy,
        tape: Option<mpsc::Sender<TapeFrame>>,
        tweak: impl FnOnce(&mut SessionConfig),
    ) -> Self {
        let server = FakeWs::start().await.unwrap();
        let (events_tx, events) = mpsc::channel(64);
        let stats = Arc::new(FeedStats::default());
        let cancel = CancellationToken::new();
        let mut cfg = SessionConfig {
            venue: Venue::Binance,
            url: server.url.clone(),
            symbols: vec![btc()],
            connect_timeout: WAIT,
            max_frame_bytes: session::DEFAULT_MAX_FRAME_BYTES,
            backoff_reset_after: session::DEFAULT_BACKOFF_RESET_AFTER,
        };
        tweak(&mut cfg);
        let out = Outputs {
            events: events_tx,
            tape,
        };
        let task = tokio::spawn(session::run(
            Arc::new(TestFeed { policy }),
            cfg,
            out,
            Arc::clone(&stats),
            cancel.clone(),
        ));
        Harness {
            server,
            events,
            stats,
            cancel,
            task,
        }
    }

    /// Accepts the next connection and checks it subscribes first.
    async fn accept_subscribed(&mut self) -> ServerWs {
        let mut ws = self.server.accept(WAIT).await.expect("no connection");
        assert_eq!(recv_text(&mut ws).await, "sub:BTC/USDT");
        ws
    }

    async fn event(&mut self) -> FeedEvent {
        timeout(WAIT, self.events.recv())
            .await
            .expect("no event in time")
            .expect("events closed")
    }

    async fn depth(&mut self) -> u64 {
        match self.event().await {
            FeedEvent::Depth { update, .. } => update.exchange_ts_ms,
            other => panic!("want depth, got {other:?}"),
        }
    }

    async fn stop(self) {
        self.cancel.cancel();
        timeout(Duration::from_secs(2), self.task)
            .await
            .expect("session did not stop within 2s")
            .unwrap();
    }
}

async fn recv(ws: &mut ServerWs) -> Message {
    timeout(WAIT, ws.next())
        .await
        .expect("no client message in time")
        .expect("client went away")
        .expect("read error")
}

async fn recv_text(ws: &mut ServerWs) -> String {
    match recv(ws).await {
        Message::Text(t) => t.as_str().to_owned(),
        other => panic!("want text, got {other:?}"),
    }
}

async fn send(ws: &mut ServerWs, text: &str) {
    ws.send(Message::text(text)).await.unwrap();
}

fn count(c: &AtomicU64) -> u64 {
    c.load(Ordering::SeqCst)
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn ac_1_depth_carries_monotonic_receive_time() {
    let mut h = Harness::start(quiet_policy(), None).await;
    let mut ws = h.accept_subscribed().await;
    let before = Instant::now();
    send(&mut ws, "u:7").await;
    match h.event().await {
        FeedEvent::Depth {
            update: u,
            received_at,
        } => {
            assert_eq!(u, update(7));
            assert!(received_at >= before && received_at <= Instant::now());
        }
        other => panic!("want depth, got {other:?}"),
    }
    h.stop().await;
}

#[tokio::test]
async fn ac_4_drop_emits_disconnected_then_reconnects_and_resubscribes() {
    let mut h = Harness::start(quiet_policy(), None).await;
    let mut ws = h.accept_subscribed().await;
    send(&mut ws, "u:1").await;
    assert_eq!(h.depth().await, 1);

    let dropped_at = Instant::now();
    drop(ws); // TCP close without a WebSocket close frame
    assert_eq!(
        h.event().await,
        FeedEvent::Disconnected {
            venue: Venue::Binance
        }
    );
    let mut ws = h.accept_subscribed().await;
    assert!(
        dropped_at.elapsed() < Duration::from_secs(5),
        "reconnect took {:?}",
        dropped_at.elapsed()
    );
    send(&mut ws, "u:2").await;
    assert_eq!(h.depth().await, 2);
    assert_eq!(count(&h.stats.reconnects), 1);
    h.stop().await;
}

#[tokio::test]
async fn ac_5_client_ping_after_idle_then_drop_without_pong() {
    let policy = SessionPolicy {
        idle_timeout: Duration::from_millis(600),
        client_ping: Some(ClientPing {
            after_idle: Duration::from_millis(300),
            payload: "ping",
        }),
        max_age: None,
    };
    let mut h = Harness::start(policy, None).await;
    let mut ws = h.accept_subscribed().await;

    let quiet_since = Instant::now();
    assert_eq!(recv_text(&mut ws).await, "ping");
    assert!(quiet_since.elapsed() >= Duration::from_millis(300));
    send(&mut ws, "pong").await; // answered: connection stays up

    assert_eq!(recv_text(&mut ws).await, "ping");
    // Not answered this time: the session must give up and reconnect.
    assert_eq!(
        h.event().await,
        FeedEvent::Disconnected {
            venue: Venue::Binance
        }
    );
    let _ws = h.accept_subscribed().await;
    assert_eq!(count(&h.stats.reconnects), 1);
    h.stop().await;
}

#[tokio::test]
async fn ac_5_server_ping_gets_same_payload_pong_and_silence_drops() {
    let policy = SessionPolicy {
        idle_timeout: Duration::from_millis(400),
        client_ping: None,
        max_age: None,
    };
    let mut h = Harness::start(policy, None).await;
    let mut ws = h.accept_subscribed().await;
    ws.send(Message::Ping(b"hb-1".to_vec().into()))
        .await
        .unwrap();
    match recv(&mut ws).await {
        Message::Pong(p) => assert_eq!(p.as_ref(), b"hb-1"),
        other => panic!("want pong, got {other:?}"),
    }
    let quiet_since = Instant::now();
    assert_eq!(
        h.event().await,
        FeedEvent::Disconnected {
            venue: Venue::Binance
        }
    );
    assert!(quiet_since.elapsed() >= Duration::from_millis(300));
    h.stop().await;
}

#[tokio::test]
async fn ac_6_rotation_subscribes_new_before_closing_old_without_disconnected() {
    let policy = SessionPolicy {
        idle_timeout: WAIT,
        client_ping: None,
        max_age: Some(Duration::from_millis(300)),
    };
    let mut h = Harness::start(policy, None).await;
    let mut old = h.accept_subscribed().await;
    send(&mut old, "u:1").await;
    assert_eq!(h.depth().await, 1);

    // The age limit opens a second, subscribed connection...
    let mut new = h.accept_subscribed().await;
    // ...while the old one still delivers.
    send(&mut old, "u:2").await;
    assert_eq!(h.depth().await, 2);
    // The new connection's first frame completes the handover; the old one is closed.
    send(&mut new, "u:3").await;
    assert_eq!(h.depth().await, 3);
    assert!(matches!(recv(&mut old).await, Message::Close(_)));
    send(&mut new, "u:4").await;
    assert_eq!(h.depth().await, 4, "no Disconnected between updates");
    assert_eq!(count(&h.stats.reconnects), 0);
    h.stop().await;
}

#[tokio::test]
async fn ac_7_malformed_frames_are_counted_and_skipped() {
    let mut h = Harness::start(quiet_policy(), None).await;
    let mut ws = h.accept_subscribed().await;
    for bad in ["not json", "u:abc", "{\"channel\":\"unknown\"}"] {
        send(&mut ws, bad).await;
    }
    ws.send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    send(&mut ws, "ack").await;
    send(&mut ws, "u:5").await;
    assert_eq!(h.depth().await, 5, "session stays connected");
    assert_eq!(count(&h.stats.malformed_frames), 4);
    assert_eq!(count(&h.stats.reconnects), 0);
    h.stop().await;
}

#[tokio::test]
async fn ac_8_oversized_frame_closes_counts_and_reconnects() {
    assert_eq!(session::DEFAULT_MAX_FRAME_BYTES, 4 << 20);
    let mut h = Harness::start_with(quiet_policy(), None, |c| c.max_frame_bytes = 1024).await;
    let mut ws = h.accept_subscribed().await;
    send(&mut ws, &"x".repeat(1025)).await;
    assert_eq!(
        h.event().await,
        FeedEvent::Disconnected {
            venue: Venue::Binance
        }
    );
    assert_eq!(count(&h.stats.oversized_frames), 1);
    let mut ws = h.accept_subscribed().await;
    send(&mut ws, "u:1").await;
    assert_eq!(h.depth().await, 1);
    h.stop().await;
}

#[tokio::test]
async fn ac_9_tape_records_every_frame_and_replays_to_the_same_updates() {
    let dir = tmp_dir("ac_9_replay");
    let (tape_tx, tape_rx) = mpsc::channel(64);
    let writer = tape::Writer::new(&dir, Venue::Binance, tape::DEFAULT_MAX_FILE_BYTES).unwrap();
    let writer = tokio::task::spawn_blocking(move || tape::run_blocking(writer, tape_rx));
    let mut h = Harness::start(quiet_policy(), Some(tape_tx)).await;
    let mut ws = h.accept_subscribed().await;

    let sent = ["u:1", "ack", "garbage", "u:2", "u:3"];
    for f in sent {
        send(&mut ws, f).await;
    }
    let mut live = Vec::new();
    for _ in 0..3 {
        match h.event().await {
            FeedEvent::Depth { update, .. } => live.push(update),
            other => panic!("want depth, got {other:?}"),
        }
    }
    h.stop().await;
    writer.await.unwrap().unwrap();

    let files: Vec<_> = std::fs::read_dir(dir.join("binance"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let frames: Vec<TapeFrame> = tape::read(&files[0])
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let texts: Vec<&str> = frames.iter().map(|f| f.frame.as_str()).collect();
    assert_eq!(texts, sent, "every text frame, in order");
    assert!(frames.iter().all(|f| f.venue == Venue::Binance));

    let feed = TestFeed {
        policy: quiet_policy(),
    };
    let replayed: Vec<DepthUpdate> = frames
        .iter()
        .filter_map(|f| feed.parse(&f.frame).ok().flatten())
        .collect();
    assert_eq!(replayed, live);
}

#[tokio::test]
async fn ac_9_full_tape_queue_drops_frames_without_blocking_the_feed() {
    // A tape queue of one that nobody drains.
    let (tape_tx, _tape_rx) = mpsc::channel(1);
    let mut h = Harness::start(quiet_policy(), Some(tape_tx)).await;
    let mut ws = h.accept_subscribed().await;
    for n in 1..=5 {
        send(&mut ws, &format!("u:{n}")).await;
    }
    for n in 1..=5 {
        assert_eq!(h.depth().await, n);
    }
    assert_eq!(count(&h.stats.tape_dropped), 4);
    h.stop().await;
}

#[tokio::test]
async fn ac_10_cancel_stops_within_2s_and_tape_is_a_valid_zstd_file() {
    let dir = tmp_dir("ac_10_shutdown");
    let (tape_tx, tape_rx) = mpsc::channel(64);
    let writer = tape::Writer::new(&dir, Venue::Binance, tape::DEFAULT_MAX_FILE_BYTES).unwrap();
    let writer = tokio::task::spawn_blocking(move || tape::run_blocking(writer, tape_rx));
    let mut h = Harness::start(quiet_policy(), Some(tape_tx)).await;
    let mut ws = h.accept_subscribed().await;
    send(&mut ws, "u:1").await;
    assert_eq!(h.depth().await, 1);

    let stop_at = Instant::now();
    h.stop().await; // asserts the 2 s bound
    timeout(Duration::from_secs(2), writer)
        .await
        .expect("tape writer did not finish")
        .unwrap()
        .unwrap();
    assert!(stop_at.elapsed() < Duration::from_secs(2));

    let file = std::fs::read_dir(dir.join("binance"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let raw = zstd::decode_all(std::fs::File::open(&file).unwrap()).unwrap();
    assert!(raw.ends_with(b"\n"));
    if let Ok(out) = std::process::Command::new("zstd")
        .arg("-tq")
        .arg(&file)
        .output()
    {
        assert!(out.status.success(), "zstd -t: {out:?}");
    }
}

#[tokio::test]
async fn ac_10_cancel_during_backoff_and_connect_stops_promptly() {
    let addr = silent_listener().await.unwrap();
    let (events_tx, _events) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let cfg = SessionConfig {
        venue: Venue::Okx,
        url: format!("ws://{addr}"),
        symbols: vec![btc()],
        connect_timeout: Duration::from_secs(30),
        max_frame_bytes: 1024,
        backoff_reset_after: session::DEFAULT_BACKOFF_RESET_AFTER,
    };
    let task = tokio::spawn(session::run(
        Arc::new(TestFeed {
            policy: quiet_policy(),
        }),
        cfg,
        Outputs {
            events: events_tx,
            tape: None,
        },
        Arc::new(FeedStats::default()),
        cancel.clone(),
    ));
    cancel.cancel();
    timeout(Duration::from_secs(2), task)
        .await
        .expect("session did not stop within 2s")
        .unwrap();
}

#[tokio::test]
async fn ac_11_websocket_connect_times_out_on_a_silent_server() {
    let addr = silent_listener().await.unwrap();
    let cfg = SessionConfig {
        venue: Venue::Okx,
        url: format!("ws://{addr}"),
        symbols: vec![btc()],
        connect_timeout: Duration::from_millis(200),
        max_frame_bytes: 1024,
        backoff_reset_after: session::DEFAULT_BACKOFF_RESET_AFTER,
    };
    let started = Instant::now();
    let err = session::connect(&cfg).await.unwrap_err();
    assert!(matches!(err, FeedError::Timeout { after, .. } if after == Duration::from_millis(200)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

fn rotating_policy(idle_timeout: Duration) -> SessionPolicy {
    SessionPolicy {
        idle_timeout,
        client_ping: None,
        max_age: Some(Duration::from_millis(300)),
    }
}

fn is_disconnected(e: &FeedEvent) -> bool {
    matches!(e, FeedEvent::Disconnected { .. })
}

#[tokio::test]
async fn ac_4_backoff_resets_after_a_long_connection() {
    assert_eq!(
        session::DEFAULT_BACKOFF_RESET_AFTER,
        Duration::from_secs(60)
    );
    let reset_after = Duration::from_millis(300);
    let mut h = Harness::start_with(quiet_policy(), None, |c| {
        c.backoff_reset_after = reset_after
    })
    .await;
    // Four short-lived connections grow the next delay to 1.6 s (jittered to 0.8–1.6 s).
    for _ in 0..4 {
        drop(h.accept_subscribed().await);
        assert!(is_disconnected(&h.event().await));
    }
    // One connection outlives the reset limit (and stays quiet without dropping)...
    let ws = h.accept_subscribed().await;
    assert!(
        timeout(reset_after * 2, h.events.recv()).await.is_err(),
        "no event while the connection is up"
    );
    drop(ws);
    let dropped_at = Instant::now();
    assert!(is_disconnected(&h.event().await));
    // ...so the next reconnect waits only the 100 ms base delay.
    let _ws = h.accept_subscribed().await;
    assert!(
        dropped_at.elapsed() < Duration::from_millis(400),
        "backoff was not reset: {:?}",
        dropped_at.elapsed()
    );
    h.stop().await;
}

#[tokio::test]
async fn ac_6_old_connection_dying_during_rotation_reports_disconnected() {
    let mut h = Harness::start(rotating_policy(WAIT), None).await;
    let mut old = h.accept_subscribed().await;
    send(&mut old, "u:1").await;
    assert_eq!(h.depth().await, 1);

    let mut new = h.accept_subscribed().await;
    drop(old); // dies before the new connection has delivered anything
    send(&mut new, "u:2").await;
    assert!(
        is_disconnected(&h.event().await),
        "updates may be missing: downstream must resync"
    );
    assert_eq!(h.depth().await, 2);
    assert_eq!(count(&h.stats.reconnects), 1);
    h.stop().await;
}

#[tokio::test]
async fn ac_6_failed_rotation_keeps_the_old_connection() {
    let mut h = Harness::start(rotating_policy(Duration::from_millis(600)), None).await;
    let mut old = h.accept_subscribed().await;
    send(&mut old, "u:1").await;
    assert_eq!(h.depth().await, 1);

    // The new connection never sends a frame; the old one stays live meanwhile.
    let mut new = h.accept_subscribed().await;
    let mut feed_rate = tokio::time::interval(Duration::from_millis(100));
    let mut n = 1;
    timeout(WAIT, async {
        loop {
            tokio::select! {
                _ = feed_rate.tick() => {
                    n += 1;
                    send(&mut old, &format!("u:{n}")).await;
                }
                _ = new.next() => break, // the session gave up on the silent connection
            }
        }
    })
    .await
    .expect("rotation never gave up");

    send(&mut old, "u:1000").await;
    loop {
        match h.event().await {
            FeedEvent::Depth { update, .. } if update.exchange_ts_ms == 1000 => break,
            FeedEvent::Depth { .. } => {}
            other => panic!("old connection must stay up, got {other:?}"),
        }
    }
    assert_eq!(count(&h.stats.reconnects), 0);
    h.stop().await;
}

#[tokio::test]
async fn ac_6_both_connections_dying_during_rotation_reconnects() {
    let mut h = Harness::start(rotating_policy(WAIT), None).await;
    let mut old = h.accept_subscribed().await;
    send(&mut old, "u:1").await;
    assert_eq!(h.depth().await, 1);

    let new = h.accept_subscribed().await;
    drop(old);
    drop(new);
    assert!(is_disconnected(&h.event().await));
    let mut ws = h.accept_subscribed().await;
    send(&mut ws, "u:2").await;
    assert_eq!(h.depth().await, 2);
    assert_eq!(count(&h.stats.reconnects), 1);
    h.stop().await;
}
