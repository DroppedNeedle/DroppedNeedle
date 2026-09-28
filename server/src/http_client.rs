//! The single factory for outbound HTTP clients.
//!
//! The factory owns timeouts, pool behavior, and the DroppedNeedle
//! User-Agent. Separate client instances appear only when that behavior
//! genuinely differs per upstream; per-provider rate policy lives in
//! `provider_policy`, and per-upstream resilience arrives with the clients
//! in a later stage.

use std::time::Duration;

use reqwest::Client;
use thiserror::Error;

/// User-Agent sent on every outbound request.
pub const USER_AGENT: &str = "DroppedNeedle/3 (+https://github.com/DroppedNeedle/DroppedNeedle)";

/// Default total request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default TCP/TLS connect timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the shared outbound client. Cheap to clone; build once at boot.
#[derive(Debug, Clone)]
pub struct HttpClientFactory {
    client: Client,
}

impl HttpClientFactory {
    /// Build the factory and its shared client.
    pub fn new() -> Result<Self, HttpClientError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }

    /// The shared client for upstreams with default behavior.
    pub fn shared(&self) -> &Client {
        &self.client
    }
}

/// Outbound client construction failure.
#[derive(Debug, Error)]
pub enum HttpClientError {
    /// The HTTP client could not be built (TLS backend or pool setup).
    #[error("cannot build HTTP client: {0}")]
    Build(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn shared_client_sends_droppedneedle_user_agent() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = HttpClientFactory::new().unwrap().shared().clone();
        let pending =
            tokio::spawn(async move { client.get(format!("http://{address}/")).send().await });

        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "client closed before sending headers");
            raw.extend_from_slice(&chunk[..read]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        pending.await.unwrap().unwrap();

        let text = String::from_utf8(raw).unwrap().to_lowercase();
        assert!(text.contains(&format!("user-agent: {}", USER_AGENT.to_lowercase())));
    }
}
