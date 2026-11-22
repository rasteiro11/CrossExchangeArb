//! `engine record` against fake venues: SIGINT and SIGTERM stop it within 2 s and leave valid
//! tapes (spec 0002 AC-10), with no network access.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use exchange_core::testkit::{FakeHttp, FakeWs, Route, ServerWs};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_secs(5);
const BINANCE_INFO: &str =
    include_str!("../../../crates/exchange-binance/tests/testdata/exchange_info_btcusdt.json");
const OKX_INSTRUMENTS: &str =
    include_str!("../../../crates/exchange-okx/tests/testdata/instruments_btc_usdt.json");
const BINANCE_FRAMES: &str =
    include_str!("../../../crates/exchange-binance/tests/testdata/ws_frames.ndjson");
const OKX_FRAMES: &str =
    include_str!("../../../crates/exchange-okx/tests/testdata/ws_frames.ndjson");

/// Accepts the engine's connection and sends it one recorded BTC/USDT update.
async fn serve_one_update(server: &mut FakeWs, frame: &str) -> ServerWs {
    let mut ws = server.accept(WAIT).await.expect("engine did not connect");
    let sub = timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
    assert!(sub.is_text(), "{sub:?}");
    ws.send(Message::text(frame)).await.unwrap();
    ws
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_10_signals_stop_the_engine_within_2s_with_valid_tapes() {
    for signal in ["INT", "TERM"] {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ac_10_sig{signal}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let rest = FakeHttp::start(vec![
            Route::new("/api/v3/exchangeInfo?symbol=BTCUSDT", 200, BINANCE_INFO),
            Route::new(
                "/api/v5/public/instruments?instType=SPOT&instId=BTC-USDT",
                200,
                OKX_INSTRUMENTS,
            ),
        ])
        .await
        .unwrap();
        let mut binance = FakeWs::start().await.unwrap();
        let mut okx = FakeWs::start().await.unwrap();
        let config = dir.join("engine.toml");
        std::fs::write(
            &config,
            format!(
                "symbols = [\"BTC/USDT\"]\ntape_dir = {tapes:?}\n\
                 [binance]\nws_url = {bw:?}\nrest_url = {rest:?}\n\
                 [okx]\nws_url = {ow:?}\nrest_url = {rest:?}\n",
                tapes = dir.join("tapes").display().to_string(),
                bw = binance.url,
                ow = okx.url,
                rest = rest.url,
            ),
        )
        .unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_engine"))
            .args(["record", "--config"])
            .arg(&config)
            .env("RUST_LOG", "info")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (line_tx, mut lines) = mpsc::channel(256);
        let stderr = child.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line_tx.blocking_send(line).is_err() {
                    break;
                }
            }
        });

        let _b = serve_one_update(&mut binance, BINANCE_FRAMES.lines().nth(1).unwrap()).await;
        let _o = serve_one_update(&mut okx, OKX_FRAMES.lines().nth(5).unwrap()).await;
        let mut first_updates = 0;
        while first_updates < 2 {
            let line = timeout(WAIT, lines.recv())
                .await
                .expect("no first update")
                .expect("engine exited");
            first_updates += usize::from(line.contains("first depth update"));
        }

        let pid = child.id().to_string();
        let (done_tx, done_rx) = oneshot::channel();
        let waiter = std::thread::spawn(move || {
            let _ = done_tx.send(child.wait());
        });
        assert!(
            Command::new("kill")
                .args([format!("-{signal}"), pid.clone()])
                .status()
                .unwrap()
                .success()
        );
        let Ok(status) = timeout(Duration::from_secs(2), done_rx).await else {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
            panic!("SIG{signal}: engine did not exit within 2s");
        };
        let status = status.unwrap().unwrap();
        assert!(status.success(), "SIG{signal}: {status:?}");
        waiter.join().unwrap();
        reader.join().unwrap();

        for (venue, frame) in [
            ("binance", BINANCE_FRAMES.lines().nth(1)),
            ("okx", OKX_FRAMES.lines().nth(5)),
        ] {
            let files: Vec<_> = std::fs::read_dir(dir.join("tapes").join(venue))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(files.len(), 1, "SIG{signal} {venue}: {files:?}");
            let mut text = String::new();
            zstd::Decoder::new(std::fs::File::open(&files[0]).unwrap())
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            let want = serde_json_escape(frame.unwrap());
            assert!(
                text.contains(&want),
                "SIG{signal} {venue}: frame not on tape"
            );
            if let Ok(out) = Command::new("zstd").arg("-tq").arg(&files[0]).output() {
                assert!(out.status.success(), "SIG{signal} {venue}: zstd -t {out:?}");
            }
        }
    }
}

/// The frame as it appears inside the tape line's JSON string.
fn serde_json_escape(frame: &str) -> String {
    frame.replace('\\', "\\\\").replace('"', "\\\"")
}
