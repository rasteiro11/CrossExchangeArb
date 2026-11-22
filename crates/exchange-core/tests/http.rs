//! REST helper against fake HTTP servers (spec 0002 AC-3, AC-11).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::time::Duration;

use domain::Venue;
use exchange_core::FeedError;
use exchange_core::http::{HttpClient, HttpTimeouts, MAX_BODY_BYTES};
use exchange_core::testkit::{FakeHttp, Route, silent_listener};
use tokio::time::Instant;

#[test]
fn ac_11_default_rest_timeouts() {
    let t = HttpTimeouts::default();
    assert_eq!(t.connect, Duration::from_secs(3));
    assert_eq!(t.total, Duration::from_secs(10));
}

#[tokio::test]
async fn ac_3_451_and_403_are_region_blocks_after_one_request() {
    for status in [451u16, 403] {
        let server = FakeHttp::start(vec![Route::new("/", status, "{}")])
            .await
            .unwrap();
        let http = HttpClient::new(HttpTimeouts::default()).unwrap();
        let err = http
            .get(
                Venue::Binance,
                &format!("{}/api/v3/exchangeInfo", server.url),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, FeedError::RegionBlocked { venue: Venue::Binance, status: s } if s == status),
            "{status}: {err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!("venue binance unreachable from this region (HTTP {status})")
        );
        assert_eq!(server.hit_count(), 1, "{status}: no retry");
    }
}

#[tokio::test]
async fn ac_11_silent_server_times_out_within_the_total_limit() {
    let addr = silent_listener().await.unwrap();
    let limit = Duration::from_millis(300);
    let http = HttpClient::new(HttpTimeouts {
        connect: limit,
        total: limit,
    })
    .unwrap();
    let started = Instant::now();
    let err = http
        .get(
            Venue::Okx,
            &format!("http://{addr}/api/v5/public/instruments"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, FeedError::Timeout { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn bodies_over_the_cap_are_refused() {
    let big = "x".repeat(MAX_BODY_BYTES + 1);
    let server = FakeHttp::start(vec![Route::new("/", 200, big)])
        .await
        .unwrap();
    let http = HttpClient::new(HttpTimeouts::default()).unwrap();
    let err = http.get(Venue::Okx, &server.url).await.unwrap_err();
    assert!(matches!(err, FeedError::Http { .. }), "{err:?}");
}
