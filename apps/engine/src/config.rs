//! `configs/engine.*.toml`: symbols, venue endpoints, tape directory and timeouts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use domain::Symbol;
use exchange_core::http::HttpTimeouts;
use serde::Deserialize;

/// What the file may contain. Unknown keys are rejected so typos fail loudly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    symbols: Vec<String>,
    #[serde(default = "default_tape_dir")]
    tape_dir: PathBuf,
    #[serde(default)]
    timeouts: Timeouts,
    #[serde(default)]
    binance: Endpoints,
    #[serde(default)]
    okx: Endpoints,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Timeouts {
    http_connect_ms: u64,
    http_total_ms: u64,
    ws_connect_ms: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            http_connect_ms: 3_000,
            http_total_ms: 10_000,
            ws_connect_ms: 5_000,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoints {
    ws_url: Option<String>,
    rest_url: Option<String>,
}

fn default_tape_dir() -> PathBuf {
    PathBuf::from("tapes")
}

/// One venue's endpoints after defaults are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueEndpoints {
    /// WebSocket URL.
    pub ws_url: String,
    /// REST base URL.
    pub rest_url: String,
}

/// Validated settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Symbols to stream on every venue.
    pub symbols: Vec<Symbol>,
    /// Root of the tape files.
    pub tape_dir: PathBuf,
    /// REST timeouts.
    pub http: HttpTimeouts,
    /// WebSocket handshake limit.
    pub ws_connect: Duration,
    /// Binance endpoints.
    pub binance: VenueEndpoints,
    /// OKX endpoints.
    pub okx: VenueEndpoints,
}

/// Reads and validates a config file.
pub fn load(path: &Path) -> anyhow::Result<Settings> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    parse(&text).with_context(|| format!("config {}", path.display()))
}

/// Parses and validates config text.
pub fn parse(text: &str) -> anyhow::Result<Settings> {
    let file: File = toml::from_str(text)?;
    if file.symbols.is_empty() {
        bail!("symbols is empty");
    }
    let symbols = file
        .symbols
        .iter()
        .map(|s| s.parse::<Symbol>())
        .collect::<Result<Vec<_>, _>>()?;
    let endpoints = |e: Endpoints, ws: &str, rest: &str| VenueEndpoints {
        ws_url: e.ws_url.unwrap_or_else(|| ws.to_owned()),
        rest_url: e.rest_url.unwrap_or_else(|| rest.to_owned()),
    };
    Ok(Settings {
        symbols,
        tape_dir: file.tape_dir,
        http: HttpTimeouts {
            connect: Duration::from_millis(file.timeouts.http_connect_ms),
            total: Duration::from_millis(file.timeouts.http_total_ms),
        },
        ws_connect: Duration::from_millis(file.timeouts.ws_connect_ms),
        binance: endpoints(
            file.binance,
            exchange_binance::WS_URL,
            exchange_binance::REST_URL,
        ),
        okx: endpoints(file.okx, exchange_okx::WS_URL, exchange_okx::REST_URL),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_11_default_timeouts() {
        let s = parse(r#"symbols = ["BTC/USDT"]"#).unwrap();
        assert_eq!(s.http.connect, Duration::from_secs(3));
        assert_eq!(s.http.total, Duration::from_secs(10));
        assert_eq!(s.ws_connect, Duration::from_secs(5));
        assert_eq!(s.binance.ws_url, exchange_binance::WS_URL);
        assert_eq!(s.okx.rest_url, exchange_okx::REST_URL);
    }

    #[test]
    fn dev_config_parses() {
        let s = parse(include_str!("../../../configs/engine.dev.toml")).unwrap();
        assert_eq!(s.symbols.len(), 3);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "symbols = []",
            r#"symbols = ["BTCUSDT"]"#,
            r#"symbols = ["BTC/USDT"]
               tape_dri = "x""#,
        ] {
            assert!(parse(bad).is_err(), "{bad} should fail");
        }
    }
}
