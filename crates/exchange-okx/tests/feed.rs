//! OKX parser, policy and instrument metadata (spec 0002 AC-1..AC-3, AC-5, AC-7).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::time::Duration;

use domain::{Decimal, Symbol, Venue};
use exchange_core::http::{HttpClient, HttpTimeouts};
use exchange_core::testkit::{FakeHttp, Route};
use exchange_core::{ClientPing, FeedError, MarketFeed, Sequence};
use exchange_okx::{OkxFeed, fetch_instruments};
use proptest::prelude::*;

/// Real frames recorded from the mainnet stream with `engine record`.
const FRAMES: &str = include_str!("testdata/ws_frames.ndjson");
const INSTRUMENTS: &str = include_str!("testdata/instruments_btc_usdt.json");
const UNKNOWN: &str = include_str!("testdata/instruments_unknown.json");

fn symbols() -> Vec<Symbol> {
    ["BTC", "ETH", "SOL"]
        .map(|b| Symbol::new(b, "USDT"))
        .to_vec()
}

fn feed() -> OkxFeed {
    OkxFeed::new(&symbols())
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

#[test]
fn ac_1_recorded_frames_normalize() {
    let feed = feed();
    let lines: Vec<&str> = FRAMES.lines().collect();
    for (i, ack) in lines[..4].iter().enumerate() {
        assert_eq!(feed.parse(ack).unwrap(), None, "notice/subscribe line {i}");
    }
    assert_eq!(feed.parse("pong").unwrap(), None);

    let snap = feed.parse(lines[4]).unwrap().unwrap();
    assert_eq!(snap.venue, Venue::Okx);
    assert_eq!(snap.symbol, Symbol::new("SOL", "USDT"));
    assert!(snap.snapshot);
    assert_eq!((snap.asks.len(), snap.bids.len()), (400, 400));
    assert_eq!(snap.asks[0].price.0.to_string(), "70.72", "exact decimal");
    assert_eq!(snap.asks[0].qty.0, dec("98.544758"));
    assert_eq!(snap.bids[0].price.0, dec("70.71"));
    assert_eq!(snap.exchange_ts_ms, 1_795_348_284_000);
    assert_eq!(
        snap.sequence,
        Sequence::Okx {
            seq_id: 32_952_575_817,
            prev_seq_id: -1,
            checksum: 0
        }
    );

    let u = feed.parse(lines[5]).unwrap().unwrap();
    assert_eq!(u.symbol, Symbol::new("BTC", "USDT"));
    assert!(!u.snapshot);
    assert_eq!(
        u.sequence,
        Sequence::Okx {
            seq_id: 84_697_921_435,
            prev_seq_id: 84_697_921_431,
            checksum: 0
        }
    );
    let removal = feed.parse(lines[6]).unwrap().unwrap();
    assert!(
        removal.bids[0].qty.0.is_zero(),
        "zero quantity = remove level"
    );

    for (i, line) in lines.iter().enumerate().skip(4) {
        let u = feed.parse(line).unwrap_or_else(|e| panic!("line {i}: {e}"));
        assert!(u.is_some(), "line {i}");
    }
}

#[test]
fn subscribes_to_books_for_every_symbol() {
    assert_eq!(
        feed().subscribe(&symbols()),
        [
            r#"{"args":[{"channel":"books","instId":"BTC-USDT"},{"channel":"books","instId":"ETH-USDT"},{"channel":"books","instId":"SOL-USDT"}],"op":"subscribe"}"#
        ]
    );
}

#[test]
fn ac_5_policy_pings_after_25s_and_drops_5s_later() {
    let p = feed().policy();
    assert_eq!(
        p.client_ping,
        Some(ClientPing {
            after_idle: Duration::from_secs(25),
            payload: "ping"
        })
    );
    assert_eq!(p.idle_timeout, Duration::from_secs(30));
    assert_eq!(p.max_age, None);
}

#[test]
fn ac_7_malformed_frames_are_errors() {
    let feed = feed();
    let ok = r#"{"arg":{"channel":"books","instId":"BTC-USDT"},"action":"update","data":[{"asks":[],"bids":[["1.5","2","0","1"]],"ts":"1","checksum":0,"seqId":2,"prevSeqId":1}]}"#;
    assert!(feed.parse(ok).unwrap().is_some());
    let cases = [
        ("invalid json", "{\"arg\":"),
        ("bare word", "ping"),
        ("missing field", &ok.replace(r#""seqId":2,"#, "")),
        ("non-numeric price", &ok.replace(r#"["1.5","#, r#"["x","#)),
        (
            "short level",
            &ok.replace(r#"["1.5","2","0","1"]"#, r#"["1.5"]"#),
        ),
        (
            "non-numeric ts",
            &ok.replace(r#""ts":"1""#, r#""ts":"soon""#),
        ),
        (
            "unknown channel",
            &ok.replace(r#""channel":"books""#, r#""channel":"trades""#),
        ),
        ("unsubscribed symbol", &ok.replace("BTC-USDT", "DOGE-USDT")),
        ("unknown action", &ok.replace(r#""update""#, r#""replace""#)),
        (
            "empty data",
            &ok.replace(r#""data":[{"#, r#""data":[],"x":[{"#),
        ),
        (
            "venue error",
            r#"{"event":"error","msg":"Wrong URL","code":"60004"}"#,
        ),
        ("unknown event", r#"{"event":"login","code":"0"}"#),
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
        line in 0usize..11, at in any::<prop::sample::Index>(), junk in "\\PC{0,8}", cut in any::<bool>()
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
async fn ac_2_instrument_metadata_from_public_instruments() {
    let server = FakeHttp::start(vec![Route::new(
        "/api/v5/public/instruments?instType=SPOT&instId=BTC-USDT",
        200,
        INSTRUMENTS,
    )])
    .await
    .unwrap();
    let got = fetch_instruments(&http().await, &server.url, &[Symbol::new("BTC", "USDT")])
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    let i = &got[0];
    assert_eq!((i.venue, i.venue_symbol.as_str()), (Venue::Okx, "BTC-USDT"));
    assert_eq!(i.tick_size.0, dec("0.1"));
    assert_eq!(i.lot_step.0, dec("0.00000001"));
    assert_eq!(i.min_qty.0, dec("0.00001"));
    assert_eq!(
        i.min_notional,
        Decimal::ZERO,
        "OKX spot has no min notional"
    );
}

#[tokio::test]
async fn ac_2_unlisted_symbol_fails_naming_it() {
    let server = FakeHttp::start(vec![Route::new(
        "/api/v5/public/instruments?instType=SPOT&instId=NOPE-USDT",
        200,
        UNKNOWN,
    )])
    .await
    .unwrap();
    let err = fetch_instruments(&http().await, &server.url, &[Symbol::new("NOPE", "USDT")])
        .await
        .unwrap_err();
    assert!(matches!(err, FeedError::UnknownSymbol { .. }), "{err:?}");
    assert_eq!(err.to_string(), "venue okx does not list symbol NOPE/USDT");
}

#[tokio::test]
async fn ac_3_region_block_fails_after_one_request() {
    let server = FakeHttp::start(vec![Route::new("/", 403, "{}")])
        .await
        .unwrap();
    let err = fetch_instruments(&http().await, &server.url, &symbols())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "venue okx unreachable from this region (HTTP 403)"
    );
    assert_eq!(server.hit_count(), 1);
}
