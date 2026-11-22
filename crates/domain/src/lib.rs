//! Domain types shared by every crate: venues, symbols, prices, quantities and instruments.
//!
//! Money is always [`Decimal`], parsed straight from the venue's strings; never a float.

use std::fmt;
use std::str::FromStr;

pub use rust_decimal::Decimal;

/// A trading venue the engine connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Venue {
    /// Binance spot.
    Binance,
    /// OKX spot.
    Okx,
}

impl Venue {
    /// Lower-case name used in config, logs and tape files.
    pub fn as_str(self) -> &'static str {
        match self {
            Venue::Binance => "binance",
            Venue::Okx => "okx",
        }
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Venue {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "binance" => Ok(Venue::Binance),
            "okx" => Ok(Venue::Okx),
            _ => Err(DomainError::UnknownVenue(s.to_owned())),
        }
    }
}

/// A venue-independent spot market such as `BTC/USDT`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol {
    /// Asset bought or sold, upper case (`BTC`).
    pub base: String,
    /// Asset prices are quoted in, upper case (`USDT`).
    pub quote: String,
}

impl Symbol {
    /// Builds a symbol, upper-casing both assets.
    pub fn new(base: &str, quote: &str) -> Self {
        Symbol {
            base: base.to_ascii_uppercase(),
            quote: quote.to_ascii_uppercase(),
        }
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.base, self.quote)
    }
}

impl FromStr for Symbol {
    type Err = DomainError;

    /// Parses the `BASE/QUOTE` form used in config.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((base, quote)) if is_asset(base) && is_asset(quote) => {
                Ok(Symbol::new(base, quote))
            }
            _ => Err(DomainError::BadSymbol(s.to_owned())),
        }
    }
}

fn is_asset(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A price in the quote asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Price(pub Decimal);

/// A quantity in the base asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Qty(pub Decimal);

impl FromStr for Price {
    type Err = DomainError;

    /// Parses a plain decimal string exactly; rejects floats in exponent form and negatives.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_non_negative(s).map(Price)
    }
}

impl FromStr for Qty {
    type Err = DomainError;

    /// Parses a plain decimal string exactly; rejects floats in exponent form and negatives.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_non_negative(s).map(Qty)
    }
}

/// Accepts only `digits[.digits]`: no sign, exponent, separators or whitespace.
fn parse_non_negative(s: &str) -> Result<Decimal, DomainError> {
    let bad = || DomainError::BadNumber(s.chars().take(32).collect());
    let (int, frac) = s.split_once('.').unwrap_or((s, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || !digits(frac) {
        return Err(bad());
    }
    Decimal::from_str_exact(s).map_err(|_| bad())
}

/// Trading rules of one symbol on one venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    /// Venue the rules come from.
    pub venue: Venue,
    /// Venue-independent symbol.
    pub symbol: Symbol,
    /// The venue's own name for the symbol (`BTCUSDT`, `BTC-USDT`).
    pub venue_symbol: String,
    /// Smallest price increment.
    pub tick_size: Price,
    /// Smallest quantity increment.
    pub lot_step: Qty,
    /// Smallest order quantity.
    pub min_qty: Qty,
    /// Smallest order value in the quote asset; zero when the venue has no such rule.
    pub min_notional: Decimal,
}

/// Invalid domain input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// Not one of the supported venues.
    #[error("unknown venue {0:?}")]
    UnknownVenue(String),
    /// Not of the form `BASE/QUOTE`.
    #[error("bad symbol {0:?}, want BASE/QUOTE")]
    BadSymbol(String),
    /// Not a plain non-negative decimal.
    #[error("bad number {0:?}")]
    BadNumber(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_parses_config_form() {
        assert_eq!(
            "btc/usdt".parse::<Symbol>().unwrap(),
            Symbol::new("BTC", "USDT")
        );
        assert_eq!(Symbol::new("BTC", "USDT").to_string(), "BTC/USDT");
        for bad in ["BTCUSDT", "/USDT", "BTC/", "BTC/US-DT", "BTC/USDT/X"] {
            assert!(bad.parse::<Symbol>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn prices_parse_exactly() {
        let p: Price = "100016.67000000".parse().unwrap();
        assert_eq!(p.0.to_string(), "100016.67000000");
        for bad in [
            "1e5",
            "-1",
            "abc",
            "",
            "1.",
            ".5",
            " 1",
            "1_0",
            "1.2.3",
            "+1",
            "99999999999999999999999999999",
        ] {
            assert!(bad.parse::<Price>().is_err(), "{bad} should not parse");
        }
    }
}
