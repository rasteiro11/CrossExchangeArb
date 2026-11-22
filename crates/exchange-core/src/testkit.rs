//! Fake venue servers on local listeners, for tests in this and the venue crates.
//! Not used by production code.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{WebSocketStream, accept_async};

/// The server side of one fake WebSocket connection.
pub type ServerWs = WebSocketStream<TcpStream>;

/// A WebSocket server that hands each accepted connection to the test.
pub struct FakeWs {
    /// `ws://127.0.0.1:<port>` to connect to.
    pub url: String,
    conns: mpsc::Receiver<ServerWs>,
}

impl FakeWs {
    /// Starts listening on a free local port.
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}", listener.local_addr()?);
        let (tx, conns) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if let Ok(ws) = accept_async(stream).await
                    && tx.send(ws).await.is_err()
                {
                    return;
                }
            }
        });
        Ok(FakeWs { url, conns })
    }

    /// The next connection, or `None` if none arrives within `limit`.
    pub async fn accept(&mut self, limit: Duration) -> Option<ServerWs> {
        tokio::time::timeout(limit, self.conns.recv())
            .await
            .ok()
            .flatten()
    }
}

/// One canned HTTP response, chosen by request-target prefix.
#[derive(Debug, Clone)]
pub struct Route {
    /// Request-target prefix, e.g. `/api/v3/exchangeInfo?symbol=BTCUSDT`.
    pub prefix: String,
    /// Status code to answer with.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

impl Route {
    /// A route answering `status` with `body`.
    pub fn new(prefix: &str, status: u16, body: impl Into<String>) -> Self {
        Route {
            prefix: prefix.to_owned(),
            status,
            body: body.into(),
        }
    }
}

/// An HTTP/1.1 server answering from a fixed route table (404 otherwise).
pub struct FakeHttp {
    /// `http://127.0.0.1:<port>` base URL.
    pub url: String,
    /// Requests served so far.
    pub hits: Arc<AtomicUsize>,
}

impl FakeHttp {
    /// Starts listening on a free local port.
    pub async fn start(routes: Vec<Route>) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let routes = Arc::new(routes);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let routes = Arc::clone(&routes);
                tokio::spawn(async move {
                    let _ = serve(stream, &routes).await;
                });
            }
        });
        Ok(FakeHttp { url, hits })
    }

    /// Requests served so far.
    pub fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

async fn serve(mut stream: TcpStream, routes: &[Route]) -> io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 << 10 {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let target = head.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = routes
        .iter()
        .find(|r| target.starts_with(&r.prefix))
        .map_or((404, String::from("{}")), |r| (r.status, r.body.clone()));
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

/// A listener that accepts TCP connections and never answers, for timeout tests.
pub async fn silent_listener() -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?.to_string();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    Ok(addr)
}
