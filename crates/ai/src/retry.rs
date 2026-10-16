use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{RequestBuilder, Response, StatusCode};

/// Retries after the first attempt, waiting 1 s, 2 s, then 4 s, or as long as
/// the endpoint asks, up to `MAX_WAIT`.
const RETRIES: u32 = 3;
const FIRST_DELAY: Duration = if cfg!(test) { Duration::from_millis(5) } else { Duration::from_secs(1) };
const MAX_WAIT: Duration = Duration::from_secs(30);

/// Sends a request, retrying when the endpoint is briefly unavailable:
/// connection failures, timeouts, rate limits, and 5xx/529 responses. Only
/// the request is retried, never a response that already started streaming.
pub(crate) async fn send(request: impl Fn() -> RequestBuilder, endpoint: &str) -> Result<Response> {
    let mut delay = FIRST_DELAY;
    for attempt in 0..=RETRIES {
        let last = attempt == RETRIES;
        let wait = match request().send().await {
            Ok(response) if !last && transient(response.status()) => asked_wait(response).await,
            Ok(response) => return Ok(response),
            Err(e) if !last && (e.is_connect() || e.is_timeout()) => None,
            Err(e) => return Err(e).with_context(|| format!("cannot reach {endpoint}")),
        };
        tokio::time::sleep(wait.unwrap_or(delay).min(MAX_WAIT)).await;
        delay *= 2;
    }
    unreachable!("the last attempt always returns")
}

fn transient(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// How long the endpoint asks us to wait: a `Retry-After` header in seconds,
/// or Gemini's `"retryDelay": "11s"` in the error body.
async fn asked_wait(response: Response) -> Option<Duration> {
    let header = response.headers().get("retry-after").and_then(|v| v.to_str().ok()?.trim().parse::<f64>().ok());
    let seconds = match header {
        Some(s) => s,
        None => retry_delay(&response.text().await.ok()?)?,
    };
    Duration::try_from_secs_f64(seconds).ok()
}

fn retry_delay(body: &str) -> Option<f64> {
    let rest = &body[body.find("\"retryDelay\"")? + "\"retryDelay\"".len()..];
    let value = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    value[..value.find('"')?].strip_suffix('s')?.parse().ok()
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

    #[test]
    fn reads_geminis_retry_delay() {
        let body = r#"[{"error": {"code": 429, "details": [{"@type": "x.RetryInfo", "retryDelay": "11.2s"}]}}]"#;
        assert_eq!(retry_delay(body), Some(11.2));
        assert_eq!(retry_delay(r#"{"error": {"code": 429}}"#), None);
    }
}
