//! The tape: every raw inbound frame, one JSON line each, in zstd files rotated hourly or by size.
//!
//! Line format: `{"recv_ns":…,"venue":"binance","frame":"<raw text>"}`. It is the replay input
//! for later specs, so readers ignore unknown extra fields.
//! Files: `<root>/<venue>/YYYY-MM-DDTHH.ndjson.zst`, then `YYYY-MM-DDTHH.1.ndjson.zst`, … when a
//! file passes the size limit or the name is already taken.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use domain::Venue;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Default size limit before a tape file rotates (uncompressed bytes).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 256 << 20;

/// One raw frame as read from the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapeFrame {
    /// Wall-clock receive time, nanoseconds since the Unix epoch.
    pub recv_ns: u64,
    /// Venue it came from.
    pub venue: Venue,
    /// The frame text, unmodified.
    pub frame: String,
}

impl TapeFrame {
    /// A frame received now.
    pub fn now(venue: Venue, frame: String) -> Self {
        let recv_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        TapeFrame {
            recv_ns,
            venue,
            frame,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Line {
    recv_ns: u64,
    venue: String,
    frame: String,
}

/// Writes one venue's frames to rotating zstd files.
pub struct Writer {
    dir: PathBuf,
    max_bytes: u64,
    open: Option<OpenFile>,
}

struct OpenFile {
    hour: String,
    written: u64,
    enc: zstd::Encoder<'static, BufWriter<File>>,
}

impl Writer {
    /// A writer for `<root>/<venue>/`, rotating after `max_bytes` uncompressed bytes.
    pub fn new(root: &Path, venue: Venue, max_bytes: u64) -> io::Result<Self> {
        let dir = root.join(venue.as_str());
        fs::create_dir_all(&dir)?;
        Ok(Writer {
            dir,
            max_bytes,
            open: None,
        })
    }

    /// Appends one frame, rotating first if the hour changed or the file is full.
    pub fn write(&mut self, frame: &TapeFrame) -> io::Result<()> {
        let mut line = serde_json::to_vec(&Line {
            recv_ns: frame.recv_ns,
            venue: frame.venue.as_str().to_owned(),
            frame: frame.frame.clone(),
        })?;
        line.push(b'\n');
        let hour = hour_key(frame.recv_ns);
        let len = line.len() as u64;
        let rotate = match &self.open {
            None => true,
            Some(f) => f.hour != hour || (f.written > 0 && f.written + len > self.max_bytes),
        };
        if rotate {
            self.finish_open()?;
            self.open = Some(self.create(&hour)?);
        }
        if let Some(f) = self.open.as_mut() {
            f.enc.write_all(&line)?;
            f.written += len;
        }
        Ok(())
    }

    /// Completes the open file so it is a valid zstd stream.
    pub fn finish(mut self) -> io::Result<()> {
        self.finish_open()
    }

    fn finish_open(&mut self) -> io::Result<()> {
        if let Some(f) = self.open.take() {
            f.enc.finish()?.flush()?;
        }
        Ok(())
    }

    fn create(&self, hour: &str) -> io::Result<OpenFile> {
        for i in 0..10_000 {
            let name = if i == 0 {
                format!("{hour}.ndjson.zst")
            } else {
                format!("{hour}.{i}.ndjson.zst")
            };
            let path = self.dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    let mut enc = zstd::Encoder::new(BufWriter::new(file), 3)?;
                    enc.include_checksum(true)?;
                    return Ok(OpenFile {
                        hour: hour.to_owned(),
                        written: 0,
                        enc,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other(format!(
            "no free tape file name for hour {hour} in {}",
            self.dir.display()
        )))
    }
}

/// Drains `rx` into `writer` until every sender is gone, then finishes the file.
/// On a write error it still finishes the open file (best effort) and returns the error, so
/// the owner can stop recording loudly. Blocking: run it with `spawn_blocking`.
pub fn run_blocking(mut writer: Writer, mut rx: mpsc::Receiver<TapeFrame>) -> io::Result<()> {
    while let Some(frame) = rx.blocking_recv() {
        if let Err(e) = writer.write(&frame) {
            let _ = writer.finish_open();
            return Err(e);
        }
    }
    writer.finish()
}

/// Reads a tape file written by [`Writer`].
pub fn read(path: &Path) -> io::Result<Reader> {
    let dec = zstd::Decoder::new(File::open(path)?)?;
    Ok(Reader {
        lines: BufReader::new(dec).lines(),
    })
}

/// Iterator over the frames of one tape file.
pub struct Reader {
    lines: io::Lines<BufReader<zstd::Decoder<'static, BufReader<File>>>>,
}

impl Iterator for Reader {
    type Item = io::Result<TapeFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = match self.lines.next()? {
            Ok(l) => l,
            Err(e) => return Some(Err(e)),
        };
        Some(parse_line(&line))
    }
}

fn parse_line(line: &str) -> io::Result<TapeFrame> {
    let l: Line = serde_json::from_str(line)?;
    let venue = l
        .venue
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(TapeFrame {
        recv_ns: l.recv_ns,
        venue,
        frame: l.frame,
    })
}

/// `YYYY-MM-DDTHH` (UTC) of a Unix time in nanoseconds.
pub fn hour_key(unix_ns: u64) -> String {
    let secs = unix_ns / 1_000_000_000;
    let (y, m, d) = civil_from_days(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}", (secs % 86_400) / 3_600)
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_key_is_utc_calendar_hour() {
        assert_eq!(hour_key(0), "1970-01-01T00");
        // 2026-11-22T14:05:00Z
        assert_eq!(hour_key(1_795_356_300 * 1_000_000_000), "2026-11-22T14");
        // 2024-02-29T23:59:59Z (leap day)
        assert_eq!(hour_key(1_709_251_199 * 1_000_000_000), "2024-02-29T23");
        // 2000-03-01T00:00:00Z
        assert_eq!(hour_key(951_868_800 * 1_000_000_000), "2000-03-01T00");
    }
}
