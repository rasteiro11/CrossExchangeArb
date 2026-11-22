//! Binance spot public market data: the diff-depth stream parser, the session policy and
//! instrument metadata from `exchangeInfo`.
//!
//! Endpoints and formats follow the Binance Spot API docs (WebSocket streams, `exchangeInfo`).

use std::collections::HashMap;
use std::time::Duration;

use domain::{Decimal, Instrument, Price, Qty, Symbol, Venue};
use exchange_core::http::HttpClient;
use exchange_core::{
    DepthUpdate, FeedError, Level, MarketFeed, ParseError, Sequence, SessionPolicy,
};
use serde::Deserialize;

/// Default combined-stream WebSocket endpoint.
pub const WS_URL: &str = "wss://stream.binance.com:9443/stream";
/// Default REST base URL.
pub const REST_URL: &str = "https://api.binance.com";

/// Binance's name for a symbol: `BTCUSDT`.
pub fn venue_symbol(s: &Symbol) -> String {
    format!("{}{}", s.base, s.quote)
}

/// The Binance side of a market-data session for a fixed set of symbols.
#[derive(Debug, Clone)]
pub struct BinanceFeed {
    by_venue_symbol: HashMap<String, Symbol>,
}

impl BinanceFeed {
    /// A feed that recognises updates for `symbols` only.
    pub fn new(symbols: &[Symbol]) -> Self {
        BinanceFeed {
            by_venue_symbol: symbols
                .iter()
                .map(|s| (venue_symbol(s), s.clone()))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    data: Option<DepthEvent<'a>>,
    error: Option<serde_json::Value>,
    id: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct DepthEvent<'a> {
    #[serde(rename = "e")]
    event: &'a str,
    #[serde(rename = "E")]
    event_time_ms: u64,
    #[serde(rename = "s")]
    symbol: &'a str,
    #[serde(rename = "U")]
    first_update_id: u64,
    #[serde(rename = "u")]
    last_update_id: u64,
    #[serde(rename = "b", borrow)]
    bids: Vec<[&'a str; 2]>,
    #[serde(rename = "a", borrow)]
    asks: Vec<[&'a str; 2]>,
}

fn levels(raw: &[[&str; 2]]) -> Result<Vec<Level>, ParseError> {
    raw.iter()
        .map(|[p, q]| {
            Ok(Level {
                price: p.parse::<Price>()?,
                qty: q.parse::<Qty>()?,
            })
        })
        .collect()
}

impl MarketFeed for BinanceFeed {
    fn subscribe(&self, symbols: &[Symbol]) -> Vec<String> {
        let params: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@depth@100ms", venue_symbol(s).to_ascii_lowercase()))
            .collect();
        vec![serde_json::json!({ "method": "SUBSCRIBE", "params": params, "id": 1 }).to_string()]
    }

    fn parse(&self, frame: &str) -> Result<Option<DepthUpdate>, ParseError> {
        let env: Envelope<'_> = serde_json::from_str(frame)?;
        let Some(ev) = env.data else {
            if let Some(err) = env.error {
                return Err(ParseError(format!("venue error: {err}")));
            }
            if env.id.is_some() {
                return Ok(None); // subscription ack: {"result":null,"id":1}
            }
            return Err(ParseError("frame has neither data nor id".into()));
        };
        if ev.event != "depthUpdate" {
            return Err(ParseError(format!("unknown event {:?}", ev.event)));
        }
        let symbol = self
            .by_venue_symbol
            .get(ev.symbol)
            .ok_or_else(|| ParseError(format!("unsubscribed symbol {:?}", ev.symbol)))?;
        Ok(Some(DepthUpdate {
            venue: Venue::Binance,
            symbol: symbol.clone(),
            bids: levels(&ev.bids)?,
            asks: levels(&ev.asks)?,
            sequence: Sequence::Binance {
                first_update_id: ev.first_update_id,
                last_update_id: ev.last_update_id,
            },
            snapshot: false,
            exchange_ts_ms: ev.event_time_ms,
        }))
    }

    fn policy(&self) -> SessionPolicy {
        SessionPolicy {
            // The server pings every 20 s; a minute of silence means the line is dead.
            idle_timeout: Duration::from_secs(60),
            client_ping: None,
            // Binance closes connections at 24 h; rotate an hour early.
            max_age: Some(Duration::from_secs(23 * 3_600)),
        }
    }
}

#[derive(Deserialize)]
struct ExchangeInfo {
    symbols: Vec<SymbolInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SymbolInfo {
    symbol: String,
    filters: Vec<Filter>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Filter {
    filter_type: String,
    tick_size: Option<String>,
    step_size: Option<String>,
    min_qty: Option<String>,
    min_notional: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    code: i64,
}

/// Binance's error code for a symbol it does not list.
const INVALID_SYMBOL: i64 = -1121;

/// Fetches trading rules for each symbol from `GET /api/v3/exchangeInfo?symbol=…`.
pub async fn fetch_instruments(
    http: &HttpClient,
    rest_url: &str,
    symbols: &[Symbol],
) -> Result<Vec<Instrument>, FeedError> {
    let mut out = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let vs = venue_symbol(symbol);
        let url = format!("{rest_url}/api/v3/exchangeInfo?symbol={vs}");
        let (status, body) = http.get(Venue::Binance, &url).await?;
        let unknown = || FeedError::UnknownSymbol {
            venue: Venue::Binance,
            symbol: symbol.clone(),
        };
        let bad = |detail: String| FeedError::Http {
            venue: Venue::Binance,
            what: format!("GET {url}"),
            detail,
        };
        if status == 400
            && serde_json::from_slice::<ApiError>(&body).is_ok_and(|e| e.code == INVALID_SYMBOL)
        {
            return Err(unknown());
        }
        if !(200..300).contains(&status) {
            let snippet = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            return Err(bad(format!("status {status}: {snippet}")));
        }
        let info: ExchangeInfo = serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?;
        let si = info
            .symbols
            .into_iter()
            .find(|s| s.symbol == vs)
            .ok_or_else(unknown)?;
        out.push(instrument(symbol, si).map_err(bad)?);
    }
    Ok(out)
}

fn instrument(symbol: &Symbol, si: SymbolInfo) -> Result<Instrument, String> {
    let find = |kind: &str| si.filters.iter().find(|f| f.filter_type == kind);
    let price_filter = find("PRICE_FILTER").ok_or("missing PRICE_FILTER")?;
    let lot = find("LOT_SIZE").ok_or("missing LOT_SIZE")?;
    let min_notional = match find("NOTIONAL").or_else(|| find("MIN_NOTIONAL")) {
        Some(f) => decimal(f.min_notional.as_deref(), "minNotional")?,
        None => Decimal::ZERO,
    };
    Ok(Instrument {
        venue: Venue::Binance,
        symbol: symbol.clone(),
        venue_symbol: si.symbol.clone(),
        tick_size: Price(decimal(price_filter.tick_size.as_deref(), "tickSize")?),
        lot_step: Qty(decimal(lot.step_size.as_deref(), "stepSize")?),
        min_qty: Qty(decimal(lot.min_qty.as_deref(), "minQty")?),
        min_notional,
    })
}

fn decimal(v: Option<&str>, field: &str) -> Result<Decimal, String> {
    let s = v.ok_or_else(|| format!("missing {field}"))?;
    s.parse::<Price>()
        .map(|p| p.0)
        .map_err(|e| format!("{field}: {e}"))
}
