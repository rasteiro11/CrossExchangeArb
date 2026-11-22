//! Binance parser, policy and instrument metadata (spec 0002 AC-1..AC-3, AC-5..AC-7).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::time::Duration;

use domain::{Decimal, Symbol, Venue};
use exchange_binance::{BinanceFeed, fetch_instruments};
use exchange_core::http::{HttpClient, HttpTimeouts};
use exchange_core::testkit::{FakeHttp, Route};
use exchange_core::{FeedError, MarketFeed, Sequence};
use proptest::prelude::*;

/// Real frames recorded from the mainnet stream with `engine record`.
const FRAMES: &str = include_str!("testdata/ws_frames.ndjson");
const EXCHANGE_INFO: &str = include_str!("testdata/exchange_info_btcusdt.json");
const UNKNOWN: &str = include_str!("testdata/exchange_info_unknown.json");

fn symbols() -> Vec<Symbol> {
    ["BTC", "ETH", "SOL"]
        .map(|b| Symbol::new(b, "USDT"))
        .to_vec()
}

fn feed() -> BinanceFeed {
    BinanceFeed::new(&symbols())
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

#[test]
fn ac_1_recorded_frames_normalize() {
    let feed = feed();
    let lines: Vec<&str> = FRAMES.lines().collect();
    assert_eq!(feed.parse(lines[0]).unwrap(), None, "subscription ack");

    let u = feed.parse(lines[1]).unwrap().unwrap();
    assert_eq!(u.venue, Venue::Binance);
    assert_eq!(u.symbol, Symbol::new("BTC", "USDT"));
    assert_eq!(u.bids.len(), 1);
    assert_eq!(
        u.bids[0].price.0.to_string(),
        "60935.04000000",
        "exact decimal"
    );
    assert_eq!(u.bids[0].qty.0, dec("0.32289"));
    assert!(u.asks.is_empty());
    assert_eq!(
        u.sequence,
        Sequence::Binance {
            first_update_id: 103_440_793_470,
            last_update_id: 103_440_793_470
        }
    );
    assert!(!u.snapshot);
    assert_eq!(u.exchange_ts_ms, 1_795_348_293_315);

    let removal = feed.parse(lines[2]).unwrap().unwrap();
    assert!(
        removal.bids[0].qty.0.is_zero(),
        "zero quantity = remove level"
    );

    for (i, line) in lines.iter().enumerate().skip(1) {
        let u = feed.parse(line).unwrap_or_else(|e| panic!("line {i}: {e}"));
        assert!(u.is_some(), "line {i}");
    }
}

#[test]
fn subscribes_to_100ms_diff_depth_for_every_symbol() {
    assert_eq!(
        feed().subscribe(&symbols()),
        [
            r#"{"id":1,"method":"SUBSCRIBE","params":["btcusdt@depth@100ms","ethusdt@depth@100ms","solusdt@depth@100ms"]}"#
        ]
    );
}

#[test]
fn ac_5_ac_6_policy_idles_out_at_60s_and_rotates_at_23h() {
    let p = feed().policy();
    assert_eq!(p.idle_timeout, Duration::from_secs(60));
    assert_eq!(p.client_ping, None, "Binance pings us; tungstenite answers");
    assert_eq!(p.max_age, Some(Duration::from_secs(23 * 3_600)));
}

#[test]
fn ac_7_malformed_frames_are_errors() {
    let feed = feed();
    let ok = r#"{"stream":"btcusdt@depth@100ms","data":{"e":"depthUpdate","E":1,"s":"BTCUSDT","U":1,"u":2,"b":[["1.0","2.0"]],"a":[]}}"#;
    assert!(feed.parse(ok).unwrap().is_some());
    let cases = [
        ("invalid json", "{\"stream\":"),
        ("not an object", "[1,2]"),
        ("missing field", &ok.replace(r#""u":2,"#, "")),
        (
            "non-numeric price",
            &ok.replace(r#"["1.0","2.0"]"#, r#"["abc","2.0"]"#),
        ),
        (
            "exponent price",
            &ok.replace(r#"["1.0","2.0"]"#, r#"["1e3","2.0"]"#),
        ),
        (
            "negative qty",
            &ok.replace(r#"["1.0","2.0"]"#, r#"["1.0","-2.0"]"#),
        ),
        ("short level", &ok.replace(r#"["1.0","2.0"]"#, r#"["1.0"]"#)),
        ("unknown event", &ok.replace("depthUpdate", "trade")),
        (
            "unsubscribed symbol",
            &ok.replace(r#""s":"BTCUSDT""#, r#""s":"DOGEUSDT""#),
        ),
        (
            "venue error",
            r#"{"error":{"code":2,"msg":"Invalid request"},"id":1}"#,
        ),
        ("no data or id", r#"{"result":null}"#),
    ];
    for (name, frame) in cases {
        assert!(feed.parse(frame).is_err(), "{name}: {frame}");
    }
}

proptest! {
    #[test]
    fn ac_7_parser_never_panics_on_arbitrary_input(s in "\\PC*") {
        let _ = feed().parse(&s);
    }

    #[test]
    fn ac_7_parser_never_panics_on_mutated_frames(
        line in 0usize..7, at in any::<prop::sample::Index>(), junk in "\\PC{0,8}", cut in any::<bool>()
    ) {
        let frame = FRAMES.lines().nth(line).unwrap();
        let i = at.index(frame.len());
        let i = (0..=i).rev().find(|&i| frame.is_char_boundary(i)).unwrap_or(0);
        let mutated = if cut { frame[..i].to_owned() } else { format!("{}{junk}{}", &frame[..i], &frame[i..]) };
        let _ = feed().parse(&mutated);
    }
}

async fn http() -> HttpClient {
    HttpClient::new(HttpTimeouts::default()).unwrap()
}

#[tokio::test]
async fn ac_2_instrument_metadata_from_exchange_info() {
    let server = FakeHttp::start(vec![Route::new(
        "/api/v3/exchangeInfo?symbol=BTCUSDT",
        200,
        EXCHANGE_INFO,
    )])
    .await
    .unwrap();
    let got = fetch_instruments(&http().await, &server.url, &[Symbol::new("BTC", "USDT")])
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    let i = &got[0];
    assert_eq!(
        (i.venue, i.venue_symbol.as_str()),
        (Venue::Binance, "BTCUSDT")
    );
    assert_eq!(i.tick_size.0, dec("0.01"));
    assert_eq!(i.lot_step.0, dec("0.00001"));
    assert_eq!(i.min_qty.0, dec("0.00001"));
    assert_eq!(i.min_notional, dec("5"));
}

#[tokio::test]
async fn ac_2_unlisted_symbol_fails_naming_it() {
    let server = FakeHttp::start(vec![
        Route::new("/api/v3/exchangeInfo?symbol=BTCUSDT", 200, EXCHANGE_INFO),
        Route::new("/api/v3/exchangeInfo?symbol=NOPEUSDT", 400, UNKNOWN),
    ])
    .await
    .unwrap();
    let err = fetch_instruments(
        &http().await,
        &server.url,
        &[Symbol::new("BTC", "USDT"), Symbol::new("NOPE", "USDT")],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, FeedError::UnknownSymbol { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "venue binance does not list symbol NOPE/USDT"
    );
}

#[tokio::test]
async fn ac_3_region_block_fails_after_one_request() {
    let server = FakeHttp::start(vec![Route::new(
        "/",
        451,
        r#"{"code":0,"msg":"restricted"}"#,
    )])
    .await
    .unwrap();
    let err = fetch_instruments(&http().await, &server.url, &symbols())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "venue binance unreachable from this region (HTTP 451)"
    );
    assert_eq!(server.hit_count(), 1);
}
