//! `droppedneedle --health-check`: the image's own health check.
//!
//! Asks this server's `/health` once, on the address it listens on, and
//! exits 0 on a 2xx answer and 1 on anything else. Docker reserves exit
//! code 2, so it is never used.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use droppedneedle_kernel::config::{AppConfig, BindHost};
use droppedneedle_kernel::http_client::HttpClientFactory;
use reqwest::Url;

/// How long the check waits for an answer, under Docker's 10 second
/// timeout. The port binds before migrations run, so a connection that is
/// accepted and never answered is the normal case during a boot.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// Run the check and exit the process with its result.
pub fn run() -> ! {
    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            println!("unhealthy: {error}");
            std::process::exit(1);
        }
    };
    let url = match probe_url(config.bind_host, config.port, &config.base_path) {
        Some(url) => url,
        None => {
            println!("unhealthy: cannot build the health address");
            std::process::exit(1);
        }
    };
    let factory = match HttpClientFactory::new() {
        Ok(factory) => factory,
        Err(error) => {
            println!("unhealthy: cannot build an HTTP client: {error}");
            std::process::exit(1);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            println!("unhealthy: cannot start async runtime: {error}");
            std::process::exit(1);
        }
    };
    let verdict = runtime.block_on(check(&factory, url, ANSWER_WITHIN));
    println!("{}", verdict.line());
    std::process::exit(if verdict.healthy() { 0 } else { 1 });
}

/// The address the check asks: the listener's own address, or loopback
/// when it listens on every address. The path is `BASE_PATH` plus
/// `/health`.
fn probe_url(bind_host: BindHost, port: u16, base_path: &str) -> Option<Url> {
    let ip = match bind_host {
        BindHost::Auto => IpAddr::V4(Ipv4Addr::LOCALHOST),
        BindHost::Fixed(ip) if ip.is_unspecified() => match ip {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        },
        BindHost::Fixed(ip) => ip,
    };
    let mut url = Url::parse("http://localhost/").ok()?;
    url.set_ip_host(ip).ok()?;
    url.set_port(Some(port)).ok()?;
    url.set_path(&format!("{base_path}/health"));
    Some(url)
}

/// What the check found.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// `/health` answered with this status.
    Answered(reqwest::StatusCode),
    /// Nothing answered within the time allowed.
    TimedOut,
    /// The connection was refused or failed.
    NoConnection,
    /// The request failed some other way.
    Failed,
}

impl Verdict {
    fn healthy(&self) -> bool {
        matches!(self, Verdict::Answered(status) if status.is_success())
    }

    fn line(&self) -> String {
        match self {
            Verdict::Answered(status) if status.is_success() => format!("healthy: {status}"),
            Verdict::Answered(status) => format!("unhealthy: /health answered {status}"),
            Verdict::TimedOut => "unhealthy: no answer in time".to_owned(),
            Verdict::NoConnection => "unhealthy: cannot connect".to_owned(),
            Verdict::Failed => "unhealthy: the request failed".to_owned(),
        }
    }
}

/// One GET, with no proxy and no redirects: a 3xx is an answer, and it
/// is not healthy. The body is never read.
async fn check(factory: &HttpClientFactory, url: Url, within: Duration) -> Verdict {
    match factory
        .direct_no_redirect()
        .get(url)
        .timeout(within)
        .send()
        .await
    {
        Ok(response) => Verdict::Answered(response.status()),
        Err(error) if error.is_timeout() => Verdict::TimedOut,
        Err(error) if error.is_connect() => Verdict::NoConnection,
        Err(_) => Verdict::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn url(bind_host: BindHost, base_path: &str) -> String {
        probe_url(bind_host, 8688, base_path)
            .map(String::from)
            .unwrap_or_default()
    }

    fn fixed(ip: &str) -> BindHost {
        BindHost::Fixed(ip.parse().unwrap())
    }

    #[test]
    fn the_probe_asks_loopback_for_a_wildcard_and_the_address_for_a_pinned_host() {
        assert_eq!(url(BindHost::Auto, ""), "http://127.0.0.1:8688/health");
        assert_eq!(url(fixed("0.0.0.0"), ""), "http://127.0.0.1:8688/health");
        assert_eq!(url(fixed("::"), ""), "http://[::1]:8688/health");
        assert_eq!(
            url(fixed("192.168.1.20"), ""),
            "http://192.168.1.20:8688/health"
        );
        assert_eq!(url(fixed("fd00::20"), ""), "http://[fd00::20]:8688/health");
        assert_eq!(
            url(BindHost::Auto, "/music"),
            "http://127.0.0.1:8688/music/health"
        );
    }

    /// A listener that answers every request with `status_line`.
    async fn answering(status_line: &'static str) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let answer = format!(
                    "HTTP/1.1 {status_line}\r\nlocation: /elsewhere\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                );
                let _ = stream.write_all(answer.as_bytes()).await;
            }
        });
        probe_url(BindHost::Auto, port, "").unwrap()
    }

    async fn verdict_for(url: Url) -> Verdict {
        check(
            &HttpClientFactory::new().unwrap(),
            url,
            Duration::from_millis(300),
        )
        .await
    }

    #[tokio::test]
    async fn only_a_2xx_answer_is_healthy() {
        let ok = verdict_for(answering("200 OK").await).await;
        assert!(ok.healthy(), "{ok:?}");

        let busy = verdict_for(answering("503 Service Unavailable").await).await;
        assert_eq!(
            busy,
            Verdict::Answered(reqwest::StatusCode::SERVICE_UNAVAILABLE)
        );
        assert!(!busy.healthy());

        let moved = verdict_for(answering("302 Found").await).await;
        assert_eq!(moved, Verdict::Answered(reqwest::StatusCode::FOUND));
        assert!(!moved.healthy());
    }

    #[tokio::test]
    async fn a_refused_connection_is_unhealthy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let verdict = verdict_for(probe_url(BindHost::Auto, port, "").unwrap()).await;
        assert_eq!(verdict, Verdict::NoConnection);
        assert!(!verdict.healthy());
    }

    #[tokio::test]
    async fn a_listener_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let verdict = verdict_for(probe_url(BindHost::Auto, port, "").unwrap()).await;
        drop(listener);
        assert_eq!(verdict, Verdict::TimedOut);
        assert!(!verdict.healthy());
    }
}
