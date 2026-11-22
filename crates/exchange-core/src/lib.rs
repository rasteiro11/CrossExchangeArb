//! Venue-independent market-data plumbing.
//!
//! A venue crate implements [`MarketFeed`] (what to subscribe to, how to parse a frame, which
//! heartbeat rules apply). [`session::run`] does the rest: connect, subscribe, read, detect dead
//! connections, reconnect with backoff, rotate long-lived connections, and record raw frames to
//! the [`tape`].

pub mod http;
pub mod session;
pub mod tape;
#[cfg(feature = "testkit")]
pub mod testkit;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use domain::{Price, Qty, Symbol, Venue};
use tokio::time::Instant;

/// One price level of a depth update. A zero quantity removes the level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    /// Level price.
    pub price: Price,
    /// Total quantity at the price; zero means "remove".
    pub qty: Qty,
}

/// Venue sequencing data a book needs to detect gaps (spec 0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// Binance diff-depth: first (`U`) and last (`u`) update id in the event.
    Binance {
        /// First update id in the event (`U`).
        first_update_id: u64,
        /// Last update id in the event (`u`).
        last_update_id: u64,
    },
    /// OKX `books`: sequence ids and the CRC32 checksum of the top 25 levels.
    Okx {
        /// Sequence id of this message.
        seq_id: i64,
        /// Sequence id of the previous message; -1 on a snapshot.
        prev_seq_id: i64,
        /// Signed CRC32 checksum the venue computed over the resulting book.
        checksum: i32,
    },
}

/// A normalized order-book change from one venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthUpdate {
    /// Venue it came from.
    pub venue: Venue,
    /// Venue-independent symbol.
    pub symbol: Symbol,
    /// Changed bid levels.
    pub bids: Vec<Level>,
    /// Changed ask levels.
    pub asks: Vec<Level>,
    /// Sequencing data for gap detection.
    pub sequence: Sequence,
    /// True when the update replaces the whole book instead of changing it.
    pub snapshot: bool,
    /// Venue event time, milliseconds since the Unix epoch.
    pub exchange_ts_ms: u64,
}

/// What a session delivers downstream.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    /// A depth update with the local, monotonic time its frame arrived.
    Depth {
        /// The update.
        update: DepthUpdate,
        /// When the frame was read from the socket.
        received_at: Instant,
    },
    /// The connection dropped; books built from earlier updates are stale until resynced.
    Disconnected {
        /// Venue whose connection dropped.
        venue: Venue,
    },
}

/// A frame that is not a usable depth update (bad JSON, missing field, bad number, unknown
/// channel or symbol). Counted and skipped, never fatal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

impl From<serde_json::Error> for ParseError {
    fn from(e: serde_json::Error) -> Self {
        ParseError(format!("json: {e}"))
    }
}

impl From<domain::DomainError> for ParseError {
    fn from(e: domain::DomainError) -> Self {
        ParseError(e.to_string())
    }
}

/// The venue-specific part of a market-data connection.
pub trait MarketFeed: Send + Sync + 'static {
    /// Text frames to send right after connecting.
    fn subscribe(&self, symbols: &[Symbol]) -> Vec<String>;
    /// Parses one text frame. `Ok(None)` is a frame that carries no depth (ack, pong).
    fn parse(&self, frame: &str) -> Result<Option<DepthUpdate>, ParseError>;
    /// Heartbeat and connection-lifetime rules.
    fn policy(&self) -> SessionPolicy;
}

/// Heartbeat and connection-lifetime rules of a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    /// No inbound frame for this long means the connection is dead.
    pub idle_timeout: Duration,
    /// Some venues want the client to ping when the line is quiet.
    pub client_ping: Option<ClientPing>,
    /// Reconnect proactively once a connection is this old.
    pub max_age: Option<Duration>,
}

/// A client-side ping sent after a quiet period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPing {
    /// Quiet time before the ping is sent.
    pub after_idle: Duration,
    /// Text frame to send.
    pub payload: &'static str,
}

/// Startup and connection errors.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    /// The venue refuses this network location (HTTP 451 or 403); retrying will not help.
    #[error("venue {venue} unreachable from this region (HTTP {status})")]
    RegionBlocked {
        /// Venue that refused.
        venue: Venue,
        /// HTTP status it answered with.
        status: u16,
    },
    /// A configured symbol the venue does not list.
    #[error("venue {venue} does not list symbol {symbol}")]
    UnknownSymbol {
        /// Venue asked.
        venue: Venue,
        /// Symbol it does not list.
        symbol: Symbol,
    },
    /// A request or connection attempt took too long.
    #[error("{what} timed out after {after:?}")]
    Timeout {
        /// What was attempted.
        what: String,
        /// The limit that was hit.
        after: Duration,
    },
    /// Any other HTTP failure.
    #[error("venue {venue} HTTP {what}: {detail}")]
    Http {
        /// Venue asked.
        venue: Venue,
        /// Request description.
        what: String,
        /// Status or transport error.
        detail: String,
    },
    /// The HTTP client could not be built (TLS setup).
    #[error("http client setup: {0}")]
    ClientSetup(String),
    /// WebSocket handshake or transport failure.
    #[error("websocket {url}: {source}")]
    WebSocket {
        /// URL connected to.
        url: String,
        /// Underlying error.
        #[source]
        source: Box<tokio_tungstenite::tungstenite::Error>,
    },
}

/// Per-venue counters, shared between the session and whoever reports them.
#[derive(Debug, Default)]
pub struct FeedStats {
    /// Text frames read.
    pub frames: AtomicU64,
    /// Frames that failed to parse.
    pub malformed_frames: AtomicU64,
    /// Frames over the size cap (each one also drops the connection).
    pub oversized_frames: AtomicU64,
    /// Connections that dropped and were retried.
    pub reconnects: AtomicU64,
    /// Frames the tape could not keep up with.
    pub tape_dropped: AtomicU64,
}

impl FeedStats {
    /// Reads one counter.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    pub(crate) fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}
