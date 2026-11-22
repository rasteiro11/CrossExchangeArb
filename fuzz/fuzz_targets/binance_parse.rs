//! Feeds arbitrary frames to the Binance parser; it must return, never panic.
#![no_main]

use domain::Symbol;
use exchange_core::MarketFeed;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(frame) = std::str::from_utf8(data) {
        let symbols = ["BTC", "ETH", "SOL"].map(|b| Symbol::new(b, "USDT"));
        let _ = exchange_binance::BinanceFeed::new(&symbols).parse(frame);
    }
});
