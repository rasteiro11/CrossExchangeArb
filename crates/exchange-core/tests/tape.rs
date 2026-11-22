//! Tape file format and rotation (spec 0002 AC-9).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::io::Read;
use std::path::{Path, PathBuf};

use domain::Venue;
use exchange_core::tape::{self, TapeFrame, Writer};

const HOUR_NS: u64 = 3_600 * 1_000_000_000;
/// 2026-11-22T14:00:00Z
const T14: u64 = 1_795_356_000 * 1_000_000_000;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn frame(recv_ns: u64, text: &str) -> TapeFrame {
    TapeFrame {
        recv_ns,
        venue: Venue::Okx,
        frame: text.to_owned(),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

fn decompressed(path: &Path) -> String {
    let mut s = String::new();
    zstd::Decoder::new(std::fs::File::open(path).unwrap())
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    s
}

#[test]
fn ac_9_one_json_line_per_frame_with_raw_text() {
    let dir = tmp_dir("ac_9_format");
    let mut w = Writer::new(&dir, Venue::Okx, tape::DEFAULT_MAX_FILE_BYTES).unwrap();
    w.write(&frame(T14 + 5, r#"{"arg":{"channel":"books"},"x":"a\"b"}"#))
        .unwrap();
    w.write(&frame(T14 + 6, "pong")).unwrap();
    w.finish().unwrap();

    let path = dir.join("okx/2026-11-22T14.ndjson.zst");
    assert_eq!(
        decompressed(&path),
        concat!(
            r#"{"recv_ns":1795356000000000005,"venue":"okx","frame":"{\"arg\":{\"channel\":\"books\"},\"x\":\"a\\\"b\"}"}"#,
            "\n",
            r#"{"recv_ns":1795356000000000006,"venue":"okx","frame":"pong"}"#,
            "\n",
        )
    );
    let back: Vec<TapeFrame> = tape::read(&path).unwrap().map(Result::unwrap).collect();
    assert_eq!(back[0].frame, r#"{"arg":{"channel":"books"},"x":"a\"b"}"#);
    assert_eq!(back[1], frame(T14 + 6, "pong"));
}

#[test]
fn ac_9_rotates_hourly_and_by_size_and_never_overwrites() {
    /// (name, max file bytes, frames as (recv_ns, text), expected file names)
    type Case<'a> = (&'a str, u64, &'a [(u64, &'a str)], &'a [&'a str]);
    let cases: [Case<'_>; 3] = [
        (
            "hourly",
            tape::DEFAULT_MAX_FILE_BYTES,
            &[(T14, "a"), (T14 + HOUR_NS - 1, "b"), (T14 + HOUR_NS, "c")],
            &["2026-11-22T14.ndjson.zst", "2026-11-22T15.ndjson.zst"],
        ),
        (
            "by size",
            // One line is ~50 bytes: two fit, the third starts a new file.
            120,
            &[(T14, "a"), (T14 + 1, "b"), (T14 + 2, "c")],
            &["2026-11-22T14.1.ndjson.zst", "2026-11-22T14.ndjson.zst"],
        ),
        (
            "oversized line still written",
            10,
            &[(T14, "a"), (T14 + 1, "b")],
            &["2026-11-22T14.1.ndjson.zst", "2026-11-22T14.ndjson.zst"],
        ),
    ];
    for (name, max_bytes, frames, want) in cases {
        let dir = tmp_dir(&format!("ac_9_rotate_{}", name.replace(' ', "_")));
        let mut w = Writer::new(&dir, Venue::Okx, max_bytes).unwrap();
        for (ns, text) in frames {
            w.write(&frame(*ns, text)).unwrap();
        }
        w.finish().unwrap();
        assert_eq!(names(&dir.join("okx")), want, "{name}");
        let total: usize = want
            .iter()
            .map(|f| tape::read(&dir.join("okx").join(f)).unwrap().count())
            .sum();
        assert_eq!(total, frames.len(), "{name}: every frame kept");
    }

    // A restart in the same hour must not clobber the earlier file.
    let dir = tmp_dir("ac_9_restart");
    for text in ["first run", "second run"] {
        let mut w = Writer::new(&dir, Venue::Okx, tape::DEFAULT_MAX_FILE_BYTES).unwrap();
        w.write(&frame(T14, text)).unwrap();
        w.finish().unwrap();
    }
    let okx = dir.join("okx");
    assert_eq!(
        names(&okx),
        ["2026-11-22T14.1.ndjson.zst", "2026-11-22T14.ndjson.zst"]
    );
    assert!(decompressed(&okx.join("2026-11-22T14.ndjson.zst")).contains("first run"));
}
