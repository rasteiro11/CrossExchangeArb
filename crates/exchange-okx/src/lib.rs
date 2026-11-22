//! OKX spot public market data: the `books` channel parser, the session policy and instrument
//! metadata from `/api/v5/public/instruments`.
//!
//! Endpoints and formats follow the OKX v5 API docs (public WebSocket, public instruments).

use std::collections::HashMap;
use std::time::Duration;

use domain::{Decimal, Instrument, Price, Qty, Symbol, Venue};
use exchange_core::http::HttpClient;
use exchange_core::{
    ClientPing, DepthUpdate, FeedError, Level, MarketFeed, ParseError, Sequence, SessionPolicy,
};
use serde::Deserialize;

/// Default public WebSocket endpoint.
pub const WS_URL: &str = "wss://ws.okx.com/ws/v5/public";
/// Default REST base URL.
pub const REST_URL: &str = "https://www.okx.com";

/// OKX's name for a symbol: `BTC-USDT`.
pub fn venue_symbol(s: &Symbol) -> String {
    format!("{}-{}", s.base, s.quote)
}

/// The OKX side of a market-data session for a fixed set of symbols.
#[derive(Debug, Clone)]
pub struct OkxFeed {
    by_venue_symbol: HashMap<String, Symbol>,
}

impl OkxFeed {
    /// A feed that recognises updates for `symbols` only.
    pub fn new(symbols: &[Symbol]) -> Self {
        OkxFeed {
            by_venue_symbol: symbols
                .iter()
                .map(|s| (venue_symbol(s), s.clone()))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    event: Option<&'a str>,
    msg: Option<String>,
    arg: Option<Arg<'a>>,
    action: Option<&'a str>,
    #[serde(borrow)]
    data: Option<Vec<Book<'a>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Arg<'a> {
    channel: &'a str,
    inst_id: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Book<'a> {
    #[serde(borrow)]
    asks: Vec<Vec<&'a str>>,
    #[serde(borrow)]
    bids: Vec<Vec<&'a str>>,
    ts: &'a str,
    checksum: i32,
    prev_seq_id: i64,
    seq_id: i64,
}

/// Each OKX level is `[price, size, deprecated, order count]`; only the first two matter.
fn levels(raw: &[Vec<&str>]) -> Result<Vec<Level>, ParseError> {
    raw.iter()
        .map(|l| match l.as_slice() {
            [p, q, ..] => Ok(Level {
                price: p.parse::<Price>()?,
                qty: q.parse::<Qty>()?,
            }),
            _ => Err(ParseError(format!("level with {} fields", l.len()))),
        })
        .collect()
}

impl MarketFeed for OkxFeed {
    fn subscribe(&self, symbols: &[Symbol]) -> Vec<String> {
        let args: Vec<serde_json::Value> = symbols
            .iter()
            .map(|s| serde_json::json!({ "channel": "books", "instId": venue_symbol(s) }))
            .collect();
        vec![serde_json::json!({ "op": "subscribe", "args": args }).to_string()]
    }

    fn parse(&self, frame: &str) -> Result<Option<DepthUpdate>, ParseError> {
        if frame == "pong" {
            return Ok(None);
        }
        let env: Envelope<'_> = serde_json::from_str(frame)?;
        match env.event {
            None => {}
            Some("subscribe" | "unsubscribe" | "channel-conn-count" | "notice") => return Ok(None),
            Some("error") => {
                return Err(ParseError(format!(
                    "venue error: {}",
                    env.msg.unwrap_or_default()
                )));
            }
            Some(other) => return Err(ParseError(format!("unknown event {other:?}"))),
        }
        let arg = env
            .arg
            .ok_or_else(|| ParseError("frame has no arg".into()))?;
        if arg.channel != "books" {
            return Err(ParseError(format!("unknown channel {:?}", arg.channel)));
        }
        let inst_id = arg
            .inst_id
            .ok_or_else(|| ParseError("arg has no instId".into()))?;
        let symbol = self
            .by_venue_symbol
            .get(inst_id)
            .ok_or_else(|| ParseError(format!("unsubscribed symbol {inst_id:?}")))?;
        let snapshot = match env.action {
            Some("snapshot") => true,
            Some("update") => false,
            other => return Err(ParseError(format!("unknown action {other:?}"))),
        };
        let mut data = env
            .data
            .ok_or_else(|| ParseError("frame has no data".into()))?;
        let book = match (data.pop(), data.is_empty()) {
            (Some(book), true) => book,
            _ => return Err(ParseError("want exactly one book in data".into())),
        };
        let exchange_ts_ms = book
            .ts
            .parse::<u64>()
            .map_err(|_| ParseError(format!("bad ts {:?}", book.ts)))?;
        Ok(Some(DepthUpdate {
            venue: Venue::Okx,
            symbol: symbol.clone(),
            bids: levels(&book.bids)?,
            asks: levels(&book.asks)?,
            sequence: Sequence::Okx {
                seq_id: book.seq_id,
                prev_seq_id: book.prev_seq_id,
                checksum: book.checksum,
            },
            snapshot,
            exchange_ts_ms,
        }))
    }

    fn policy(&self) -> SessionPolicy {
        SessionPolicy {
            // OKX closes connections idle for 30 s; ping after 25 s and expect "pong".
            idle_timeout: Duration::from_secs(30),
            client_ping: Some(ClientPing {
                after_idle: Duration::from_secs(25),
                payload: "ping",
            }),
            max_age: None,
        }
    }
}

#[derive(Deserialize)]
struct InstrumentsResponse {
    code: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Vec<InstrumentInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstrumentInfo {
    inst_id: String,
    tick_sz: String,
    lot_sz: String,
    min_sz: String,
}

/// OKX's error code for an instrument it does not list.
const INSTRUMENT_NOT_FOUND: &str = "51001";

/// Fetches trading rules for each symbol from
/// `GET /api/v5/public/instruments?instType=SPOT&instId=…`. OKX has no spot min-notional rule,
/// so `min_notional` is zero.
pub async fn fetch_instruments(
    http: &HttpClient,
    rest_url: &str,
    symbols: &[Symbol],
) -> Result<Vec<Instrument>, FeedError> {
    let mut out = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let vs = venue_symbol(symbol);
        let url = format!("{rest_url}/api/v5/public/instruments?instType=SPOT&instId={vs}");
        let (status, body) = http.get(Venue::Okx, &url).await?;
        let unknown = || FeedError::UnknownSymbol {
            venue: Venue::Okx,
            symbol: symbol.clone(),
        };
        let bad = |detail: String| FeedError::Http {
            venue: Venue::Okx,
            what: format!("GET {url}"),
            detail,
        };
        let resp: InstrumentsResponse = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) if (200..300).contains(&status) => return Err(bad(e.to_string())),
            Err(_) => {
                let snippet = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
                return Err(bad(format!("status {status}: {snippet}")));
            }
        };
        if resp.code == INSTRUMENT_NOT_FOUND {
            return Err(unknown());
        }
        if resp.code != "0" || !(200..300).contains(&status) {
            return Err(bad(format!(
                "status {status}, code {}: {}",
                resp.code, resp.msg
            )));
        }
        let info = resp
            .data
            .into_iter()
            .find(|i| i.inst_id == vs)
            .ok_or_else(unknown)?;
        let dec = |v: &str, field: &str| {
            v.parse::<Price>()
                .map(|p| p.0)
                .map_err(|e| bad(format!("{field}: {e}")))
        };
        out.push(Instrument {
            venue: Venue::Okx,
            symbol: symbol.clone(),
            venue_symbol: info.inst_id.clone(),
            tick_size: Price(dec(&info.tick_sz, "tickSz")?),
            lot_step: Qty(dec(&info.lot_sz, "lotSz")?),
            min_qty: Qty(dec(&info.min_sz, "minSz")?),
            min_notional: Decimal::ZERO,
        });
    }
    Ok(out)
}
