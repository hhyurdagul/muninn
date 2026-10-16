use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{RequestBuilder, Response, StatusCode};

/// Retries after the first attempt, waiting 1 s, 2 s, then 4 s.
const RETRIES: u32 = 3;
const FIRST_DELAY: Duration = if cfg!(test) { Duration::from_millis(5) } else { Duration::from_secs(1) };

/// Sends a request, retrying when the endpoint is briefly unavailable:
/// connection failures, timeouts, rate limits, and 5xx/529 responses. Only
/// the request is retried, never a response that already started streaming.
pub(crate) async fn send(request: impl Fn() -> RequestBuilder, endpoint: &str) -> Result<Response> {
    let mut delay = FIRST_DELAY;
    for attempt in 0..=RETRIES {
        let last = attempt == RETRIES;
        match request().send().await {
            Ok(response) if !last && transient(response.status()) => {}
            Ok(response) => return Ok(response),
            Err(e) if !last && (e.is_connect() || e.is_timeout()) => {}
            Err(e) => return Err(e).with_context(|| format!("cannot reach {endpoint}")),
        }
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    unreachable!("the last attempt always returns")
}

fn transient(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// Answers with `statuses` in order, then 200.
    async fn server(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let status = statuses.get(n).copied().unwrap_or(200);
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let reply = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        (url, count)
    }

    #[tokio::test]
    async fn retries_transient_failures_but_not_client_errors() {
        let http = reqwest::Client::new();
        let (url, count) = server(vec![502, 529]).await;
        let response = send(|| http.get(&url), &url).await.unwrap();
        assert_eq!((response.status().as_u16(), count.load(Ordering::SeqCst)), (200, 3));

        let (url, count) = server(vec![401]).await;
        let response = send(|| http.get(&url), &url).await.unwrap();
        assert_eq!((response.status().as_u16(), count.load(Ordering::SeqCst)), (401, 1));

        let (url, count) = server(vec![503; 9]).await;
        let response = send(|| http.get(&url), &url).await.unwrap();
        assert_eq!(response.status().as_u16(), 503, "gives up and reports the last answer");
        assert_eq!(count.load(Ordering::SeqCst), 1 + RETRIES as usize);
    }
}
