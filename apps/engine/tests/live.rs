//! Live smoke test against the real venues (spec 0002 AC-12, plus AC-10 on SIGINT).
//! Ignored by default: `cargo test -p engine -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs network access to Binance and OKX"]
fn ac_12_record_streams_every_venue_symbol_pair_within_10s() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ac_12_live");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dev = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../configs/engine.dev.toml"
    ))
    .unwrap();
    let tapes = dir.join("tapes");
    let config = dir.join("engine.toml");
    std::fs::write(
        &config,
        dev.replace(
            r#"tape_dir = "tapes""#,
            &format!("tape_dir = {:?}", tapes.display().to_string()),
        ),
    )
    .unwrap();

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(["record", "--config"])
        .arg(&config)
        .env("RUST_LOG", "info")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let want: HashSet<String> = ["binance", "okx"]
        .iter()
        .flat_map(|v| ["BTC/USDT", "ETH/USDT", "SOL/USDT"].map(|s| format!("venue={v} symbol={s}")))
        .collect();
    let mut seen = HashSet::new();
    let deadline = started + Duration::from_secs(10);
    while seen != want {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(left) else {
            let _ = child.kill();
            panic!("after 10s saw only {seen:?}");
        };
        if line.contains("first depth update")
            && let Some(pair) = want.iter().find(|p| line.contains(p.as_str()))
        {
            seen.insert(pair.clone());
        }
    }

    let pid = child.id().to_string();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = done_tx.send(child.wait());
    });
    let status = Command::new("kill").args(["-INT", &pid]).status().unwrap();
    assert!(status.success());
    let Ok(code) = done_rx.recv_timeout(Duration::from_secs(2)) else {
        let _ = Command::new("kill").args(["-KILL", &pid]).status();
        panic!("engine did not exit within 2s of SIGINT");
    };
    let code = code.unwrap();
    assert!(code.success(), "{code:?}");
    waiter.join().unwrap();
    reader.join().unwrap();

    for venue in ["binance", "okx"] {
        let files: Vec<_> = std::fs::read_dir(tapes.join(venue))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(!files.is_empty(), "{venue}: no tape");
        for f in files {
            let out = Command::new("zstd").arg("-tq").arg(&f).output().unwrap();
            assert!(out.status.success(), "zstd -t {}: {out:?}", f.display());
        }
    }
}
