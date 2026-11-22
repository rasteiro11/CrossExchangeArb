//! REST GET helper with timeouts, a body cap and venue-aware status mapping.

use std::time::Duration;

use domain::Venue;

use crate::FeedError;

/// Largest REST body accepted from a venue.
pub const MAX_BODY_BYTES: usize = 4 << 20;

/// HTTP timeouts. Defaults: 3 s to connect, 10 s per request in total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpTimeouts {
    /// TCP + TLS connect limit.
    pub connect: Duration,
    /// Whole-request limit, body included.
    pub total: Duration,
}

impl Default for HttpTimeouts {
    fn default() -> Self {
        HttpTimeouts {
            connect: Duration::from_secs(3),
            total: Duration::from_secs(10),
        }
    }
}

/// A REST client for venue metadata calls.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    timeouts: HttpTimeouts,
}

impl HttpClient {
    /// Builds a client with the given timeouts.
    pub fn new(timeouts: HttpTimeouts) -> Result<Self, FeedError> {
        let inner = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.total)
            .user_agent("cross-exchange-arb/0.1")
            .build()
            .map_err(|e| FeedError::ClientSetup(e.to_string()))?;
        Ok(HttpClient { inner, timeouts })
    }

    /// GETs `url` and returns the status and body. Maps HTTP 451/403 to
    /// [`FeedError::RegionBlocked`]; any other status is returned for the caller to judge.
    pub async fn get(&self, venue: Venue, url: &str) -> Result<(u16, Vec<u8>), FeedError> {
        let transport = |e: reqwest::Error| self.error(venue, url, e);
        let mut resp = self.inner.get(url).send().await.map_err(transport)?;
        let status = resp.status().as_u16();
        if status == 451 || status == 403 {
            return Err(FeedError::RegionBlocked { venue, status });
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(transport)? {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(FeedError::Http {
                    venue,
                    what: format!("GET {url}"),
                    detail: format!("body over {MAX_BODY_BYTES} bytes"),
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }

    fn error(&self, venue: Venue, url: &str, e: reqwest::Error) -> FeedError {
        if e.is_timeout() {
            return FeedError::Timeout {
                what: format!("GET {url}"),
                after: self.timeouts.total,
            };
        }
        FeedError::Http {
            venue,
            what: format!("GET {url}"),
            detail: e.to_string(),
        }
    }
}
